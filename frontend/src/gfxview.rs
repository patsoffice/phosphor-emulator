//! Interactive charset / sprite GFX viewer.
//!
//! Displays the tile/sprite sheets a machine exposes via
//! [`MachineCore::gfx_sheets`] — the caches it already decoded from ROM. Any
//! working tile-based machine is viewable "for free"; no per-machine
//! registration. Compositing reuses [`phosphor_core::gfx::render_sheet`], the
//! same code the offline `disasm gfxview` PNG export uses, so the two stay
//! pixel-identical.
//!
//! Colors come from the machine's own palette, indexed at pen group 0 — per-tile
//! color attributes aren't known without live video RAM.

use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;

use phosphor_core::core::machine::{FrontendMachine, GfxSheet};
use phosphor_core::gfx::{SheetConfig, render_sheet};

const INITIAL_WIN_W: u32 = 1024;
const INITIAL_WIN_H: u32 = 768;
const MAX_ZOOM: usize = 16;

/// Columns that make a scale-1 sheet roughly 256 px wide, so wide sprites get
/// fewer per row than 8×8 tiles.
fn cols_for(tile_width: usize) -> usize {
    (256 / tile_width.max(1)).max(1)
}

/// Launch the interactive viewer for `machine`'s decoded GFX sheets.
///
/// `initial_region` selects the sheet to open first (falls back to the first
/// exposed sheet). Returns an error string for the caller to print; the window
/// runs until the user quits. Borrows `machine` for the lifetime of the window.
pub fn run(
    machine_name: &str,
    machine: &dyn FrontendMachine,
    initial_region: Option<&str>,
) -> Result<(), String> {
    let sheets = machine.gfx_sheets();
    if sheets.is_empty() {
        return Err(format!(
            "machine '{machine_name}' exposes no GFX sheets (only tile/sprite \
             machines do; vector and bitmap-framebuffer machines have none)"
        ));
    }

    let mut current = initial_region
        .and_then(|name| sheets.iter().position(|s| s.name == name))
        .unwrap_or(0);

    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    // Nearest-neighbor when SDL scales the texture (crisp pixels, not blur).
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");

    let window = video
        .window(
            &title(machine_name, &sheets[current], current, sheets.len(), 1),
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

    let mut zoom = fit_zoom(&sheets[current], INITIAL_WIN_W, INITIAL_WIN_H);
    let mut dirty = true;
    let mut composed = compose(&sheets[current], zoom);
    let mut texture = make_texture(&tc, &composed)?;

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
                        current = (current + 1) % sheets.len();
                        let (w, h) = win_size(&canvas);
                        zoom = fit_zoom(&sheets[current], w, h);
                        dirty = true;
                    }
                    Keycode::Left | Keycode::Up | Keycode::P => {
                        current = (current + sheets.len() - 1) % sheets.len();
                        let (w, h) = win_size(&canvas);
                        zoom = fit_zoom(&sheets[current], w, h);
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
                        zoom = fit_zoom(&sheets[current], w, h);
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
            composed = compose(&sheets[current], zoom);
            texture = make_texture(&tc, &composed)?;
            canvas
                .window_mut()
                .set_title(&title(
                    machine_name,
                    &sheets[current],
                    current,
                    sheets.len(),
                    zoom,
                ))
                .ok();
            dirty = false;
        }

        // Center the sheet in the window (clipped if larger than the window).
        let (win_w, win_h) = win_size(&canvas);
        let dst = Rect::new(
            (win_w as i32 - composed.width as i32) / 2,
            (win_h as i32 - composed.height as i32) / 2,
            composed.width,
            composed.height,
        );
        canvas.set_draw_color(Color::RGB(20, 20, 24));
        canvas.clear();
        canvas.copy(&texture, None, Some(dst))?;
        canvas.present();

        std::thread::sleep(std::time::Duration::from_millis(16));
    }

    Ok(())
}

/// Composite one sheet at the given integer zoom.
fn compose(sheet: &GfxSheet<'_>, zoom: usize) -> phosphor_core::gfx::Sheet {
    render_sheet(
        sheet.cache,
        sheet.palette,
        &SheetConfig {
            cols: cols_for(sheet.cache.width()),
            scale: zoom,
        },
    )
}

/// Integer zoom that fits `sheet`'s scale-1 image inside `win_w × win_h` (min 1).
fn fit_zoom(sheet: &GfxSheet<'_>, win_w: u32, win_h: u32) -> usize {
    let cols = cols_for(sheet.cache.width());
    let rows = sheet.cache.count().div_ceil(cols);
    let base_w = (cols * sheet.cache.width()).max(1);
    let base_h = (rows * sheet.cache.height()).max(1);
    (win_w as usize / base_w)
        .min(win_h as usize / base_h)
        .clamp(1, MAX_ZOOM)
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

fn title(machine: &str, sheet: &GfxSheet<'_>, idx: usize, total: usize, zoom: usize) -> String {
    format!(
        "phosphor gfxview — {machine} / {} [{}/{}]   {} × {}×{}   {zoom}×   \
         (←/→ region, +/- zoom, 0 fit, Esc quit)",
        sheet.name,
        idx + 1,
        total,
        sheet.cache.count(),
        sheet.cache.width(),
        sheet.cache.height(),
    )
}
