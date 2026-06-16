use crate::core::debug::Debuggable;
use crate::core::save_state::Saveable;

/// Common interface for emulated hardware peripheral devices.
///
/// Every peripheral device (PIA, sound chip, blitter, etc.) implements
/// this trait to establish a uniform contract for debug inspection,
/// save/load state, power-on reset, and optional register access.
///
/// Devices are owned as concrete types by board structs and used via
/// direct method calls (inherent methods shadow trait methods in normal
/// calls, so no existing call sites need updating).
pub trait Device: Debuggable + Saveable {
    /// Human-readable device type name (e.g., "AY-8910", "PIA 6820").
    fn name(&self) -> &'static str;

    /// Reset to power-on state. Configuration (clock rates, ROM data,
    /// variant selection) is preserved; only runtime state is cleared.
    fn reset(&mut self);

    /// Read a device register by offset. Default returns 0xFF (no register file).
    fn read(&mut self, _offset: u16) -> u8 {
        0xFF
    }

    /// Write a device register by offset. Default is a no-op.
    fn write(&mut self, _offset: u16, _data: u8) {}

    /// Advance the device by one clock tick. Default is a no-op.
    fn tick(&mut self) {}
}

pub mod avg;
pub mod ay8910;
pub mod cmos_ram;
pub mod dac;
pub mod dkong_discrete;
pub mod dvg;
pub mod er2055;
pub mod i8257;
pub mod mathbox;
pub mod namco06;
pub mod namco51;
pub mod namco51_lle;
pub mod namco53;
pub mod namco_wsg;
pub mod output_latch;
pub mod pia6820;
pub mod pokey;
pub mod riot6532;
pub mod ssio;
pub mod votrax_sc01;
pub mod williams_blitter;
pub mod z80ctc;

pub use avg::{Avg, AvgVariant};
pub use ay8910::Ay8910;
pub use cmos_ram::CmosRam;
pub use dac::Mc1408Dac;
pub use dkong_discrete::DkongDiscrete;
pub use dvg::Dvg;
pub use er2055::Er2055;
pub use i8257::I8257;
pub use mathbox::Mathbox;
pub use namco_wsg::NamcoWsg;
pub use namco06::Namco06;
pub use namco51::Namco51;
pub use namco51_lle::Namco51Lle;
pub use namco53::Namco53;
pub use output_latch::OutputLatch;
pub use pia6820::Pia6820;
pub use pokey::Pokey;
pub use riot6532::Riot6532;
pub use ssio::SsioBoard;
pub use votrax_sc01::VotraxSc01;
pub use williams_blitter::WilliamsBlitter;
pub use z80ctc::Z80Ctc;
