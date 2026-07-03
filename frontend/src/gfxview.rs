//! Interactive charset / sprite GFX viewer.
//!
//! Renders a machine's registered [`GfxRegion`]s (see
//! `phosphor_machines::gfx_registry`) as on-screen tile/sprite sheets, decoded
//! straight from the ROM set with no running machine. It reuses the same
//! compositing as the offline `disasm gfxview` PNG export
//! ([`phosphor_core::gfx::render_sheet`]) so the two stay pixel-identical; the
//! only difference is this one is interactive — cycle regions, zoom, refit.
//!
//! Like the CLI export, colors come from each region's PROM palette (or a
//! grayscale ramp when the machine has none), indexed at pen group 0 — tile
//! color attributes aren't known without live VRAM.

use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;

use phosphor_core::gfx::{GfxCache, SheetConfig, decode_gfx, grayscale_ramp, render_sheet};
use phosphor_machines::gfx_registry::{self, GfxRegion};
use phosphor_machines::rom_loader::RomSet;

const INITIAL_WIN_W: u32 = 1024;
const INITIAL_WIN_H: u32 = 768;
const MAX_ZOOM: usize = 16;

/// One region decoded and ready to composite at any zoom.
struct RegionView {
    region: &'static str,
    cache: GfxCache,
    palette: Vec<(u8, u8, u8)>,
    /// Whether the palette came from a color PROM (vs. the grayscale fallback).
    prom_palette: bool,
}

impl RegionView {
    /// Decode a region's ROM bytes and resolve its palette (PROM or grayscale).
    fn build(region: &'static GfxRegion, rom_set: &RomSet) -> Result<Self, String> {
        let bytes = (region.load)(rom_set)
            .map_err(|e| format!("loading gfx region '{}': {e}", region.region))?;
        let cache = decode_gfx(&bytes, 0, region.count as usize, region.layout);

        let (palette, prom_palette) = match region.palette {
            Some(build) => match build(rom_set) {
                Ok(pal) => (pal, true),
                Err(e) => {
                    eprintln!(
                        "gfxview: palette for '{}' failed ({e}); using grayscale",
                        region.region
                    );
                    (
                        grayscale_ramp(1 << region.layout.plane_offsets.len()),
                        false,
                    )
                }
            },
            None => (
                grayscale_ramp(1 << region.layout.plane_offsets.len()),
                false,
            ),
        };

        Ok(Self {
            region: region.region,
            cache,
            palette,
            prom_palette,
        })
    }

    /// Columns that make the scale-1 sheet roughly 256 px wide, so wide sprites
    /// get fewer per row than 8×8 tiles.
    fn cols(&self) -> usize {
        (256 / self.cache.width().max(1)).max(1)
    }
}

/// Launch the interactive viewer for `machine_name` against `rom_set`.
///
/// `initial_region` selects the region to open first (falls back to the first
/// registered region). Returns an error string for the caller to print; the
/// window runs until the user quits.
pub fn run(
    machine_name: &str,
    rom_set: &RomSet,
    initial_region: Option<&str>,
) -> Result<(), String> {
    let regions = gfx_registry::regions_for(machine_name);
    if regions.is_empty() {
        return Err(format!(
            "no gfx regions registered for machine '{machine_name}'"
        ));
    }

    // Decode every region up front; skip (with a warning) any whose ROMs fail.
    let mut views: Vec<RegionView> = Vec::new();
    for r in &regions {
        match RegionView::build(r, rom_set) {
            Ok(v) => views.push(v),
            Err(e) => eprintln!("gfxview: skipping region — {e}"),
        }
    }
    if views.is_empty() {
        return Err(format!(
            "no gfx region for '{machine_name}' could be decoded"
        ));
    }

    let mut current = initial_region
        .and_then(|name| views.iter().position(|v| v.region == name))
        .unwrap_or(0);

    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    // Nearest-neighbor when SDL scales the texture (crisp pixels, not blur).
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");

    let window = video
        .window(
            &title(machine_name, &views[current], current, views.len(), 1),
            INITIAL_WIN_W,
            INITIAL_WIN_H,
        )
        .position_centered()
        .resizable()
        .build()
        .map_err(|e| e.to_string())?;
    let mut canvas = window.into_canvas().build().map_err(|e| e.to_string())?;
    let tc = canvas.texture_creator();
    let mut pump = sdl.event_pump()?;

    let mut zoom: usize = fit_zoom(&views[current], INITIAL_WIN_W, INITIAL_WIN_H);
    let mut dirty = true;

    // Composited sheet + its texture, rebuilt whenever region/zoom/size changes.
    let mut sheet = render_sheet(
        &views[current].cache,
        &views[current].palette,
        &SheetConfig {
            cols: views[current].cols(),
            scale: zoom,
        },
    );
    let mut texture = make_texture(&tc, &sheet)?;

    'main: loop {
        for event in pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape | Keycode::Q),
                    ..
                } => break 'main,

                Event::KeyDown {
                    keycode: Some(key), ..
                } => match key {
                    Keycode::Right | Keycode::Down | Keycode::Tab | Keycode::N => {
                        current = (current + 1) % views.len();
                        zoom = fit_zoom(&views[current], win_size(&canvas).0, win_size(&canvas).1);
                        dirty = true;
                    }
                    Keycode::Left | Keycode::Up | Keycode::P => {
                        current = (current + views.len() - 1) % views.len();
                        zoom = fit_zoom(&views[current], win_size(&canvas).0, win_size(&canvas).1);
                        dirty = true;
                    }
                    Keycode::Plus | Keycode::Equals | Keycode::KpPlus => {
                        zoom = (zoom + 1).min(MAX_ZOOM);
                        dirty = true;
                    }
                    Keycode::Minus | Keycode::KpMinus => {
                        zoom = zoom.saturating_sub(1).max(1);
                        dirty = true;
                    }
                    Keycode::Num0 => {
                        let (w, h) = win_size(&canvas);
                        zoom = fit_zoom(&views[current], w, h);
                        dirty = true;
                    }
                    _ => {}
                },

                Event::Window {
                    win_event: WindowEvent::SizeChanged(..) | WindowEvent::Resized(..),
                    ..
                } => dirty = true,

                _ => {}
            }
        }

        if dirty {
            let view = &views[current];
            sheet = render_sheet(
                &view.cache,
                &view.palette,
                &SheetConfig {
                    cols: view.cols(),
                    scale: zoom,
                },
            );
            texture = make_texture(&tc, &sheet)?;
            canvas
                .window_mut()
                .set_title(&title(machine_name, view, current, views.len(), zoom))
                .ok();
            dirty = false;
        }

        // Center the sheet in the window (clipped if larger than the window).
        let (win_w, win_h) = win_size(&canvas);
        let dst = Rect::new(
            (win_w as i32 - sheet.width as i32) / 2,
            (win_h as i32 - sheet.height as i32) / 2,
            sheet.width,
            sheet.height,
        );
        canvas.set_draw_color(Color::RGB(20, 20, 24));
        canvas.clear();
        canvas.copy(&texture, None, Some(dst))?;
        canvas.present();

        std::thread::sleep(std::time::Duration::from_millis(16));
    }

    Ok(())
}

/// Integer zoom that fits `view`'s scale-1 sheet inside `win_w × win_h` (min 1).
fn fit_zoom(view: &RegionView, win_w: u32, win_h: u32) -> usize {
    let cols = view.cols();
    let rows = view.cache.count().div_ceil(cols);
    let base_w = (cols * view.cache.width()).max(1);
    let base_h = (rows * view.cache.height()).max(1);
    let z = ((win_w as usize / base_w).min(win_h as usize / base_h)).max(1);
    z.min(MAX_ZOOM)
}

fn win_size(canvas: &sdl2::render::WindowCanvas) -> (u32, u32) {
    canvas.window().size()
}

fn make_texture<'a>(
    tc: &'a sdl2::render::TextureCreator<sdl2::video::WindowContext>,
    sheet: &phosphor_core::gfx::Sheet,
) -> Result<sdl2::render::Texture<'a>, String> {
    let mut tex = tc
        .create_texture_streaming(PixelFormatEnum::RGB24, sheet.width, sheet.height)
        .map_err(|e| e.to_string())?;
    tex.update(None, &sheet.rgb, sheet.width as usize * 3)
        .map_err(|e| e.to_string())?;
    Ok(tex)
}

fn title(machine: &str, view: &RegionView, idx: usize, total: usize, zoom: usize) -> String {
    format!(
        "phosphor gfxview — {machine} / {} [{}/{}]   {} × {}×{}   {}   {zoom}×   \
         (←/→ region, +/- zoom, 0 fit, Esc quit)",
        view.region,
        idx + 1,
        total,
        view.cache.count(),
        view.cache.width(),
        view.cache.height(),
        if view.prom_palette {
            "PROM palette"
        } else {
            "grayscale"
        },
    )
}
