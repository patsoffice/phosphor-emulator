//! Shared flag and signal helpers for all CPU implementations.
//!
//! Each CPU has its own flag enum (`CcFlag`, `StatusFlag`, `PswFlag`, `Flag`)
//! with identical bit-set/clear logic. These free functions deduplicate that
//! logic while leaving per-CPU wrapper methods in place for ergonomics.

/// Set or clear a single flag bit in a status register.
///
/// `flag` is any type that converts to `u8` (all CPU flag enums are `#[repr(u8)]`).
/// If `set` is true the bit is set; otherwise cleared.
#[inline]
pub fn set_flag<F: Into<u8>>(dest: &mut u8, flag: F, set: bool) {
    let mask = flag.into();
    if set {
        *dest |= mask;
    } else {
        *dest &= !mask;
    }
}

/// Test whether a single flag bit is set in a status register.
#[inline]
pub fn flag_is_set<F: Into<u8>>(src: u8, flag: F) -> bool {
    src & flag.into() != 0
}

/// Set or clear a single flag bit in a 16-bit status register.
///
/// 16-bit counterpart of [`set_flag`] for CPUs whose status register is
/// wider than a byte (M68000 SR, i8088 FLAGS).
#[inline]
pub fn set_flag_u16<F: Into<u16>>(dest: &mut u16, flag: F, set: bool) {
    let mask = flag.into();
    if set {
        *dest |= mask;
    } else {
        *dest &= !mask;
    }
}

/// Test whether a single flag bit is set in a 16-bit status register.
#[inline]
pub fn flag_is_set_u16<F: Into<u16>>(src: u16, flag: F) -> bool {
    src & flag.into() != 0
}

/// Detect a rising edge on a boolean signal, updating the previous state.
///
/// Returns `true` when `current` is high and `*previous` was low.
/// Updates `*previous` to `current` for the next call.
#[inline]
pub fn detect_rising_edge(current: bool, previous: &mut bool) -> bool {
    let edge = current && !*previous;
    *previous = current;
    edge
}
