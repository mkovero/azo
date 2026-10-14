use windows_core::PCWSTR;
use crate::win::HWND;

/// <https://github.com/microsoft/windows-rs/issues/3589>
#[expect(non_snake_case, reason = "match Windows SDK")]
pub const fn MAKEINTATOM(value: u16) -> PCWSTR {
    PCWSTR::from_raw(value as _)
}

/// A [`HWND`] that is [`Send`] and [`Sync`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SendHWND(pub HWND);

// SAFETY:
// This should be in windows-rs tbh
unsafe impl Send for SendHWND {}
// SAFETY: a window handle is a plain identifier, and the only calls made with it from other
// threads (`SendMessageW`, `PostMessageW`) are themselves safe to make from any thread, which
// is what [`Host`](super::Host) relies on to hand out [`Proxy`](super::Proxy)s to all threads.
unsafe impl Sync for SendHWND {}