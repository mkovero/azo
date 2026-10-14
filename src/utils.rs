use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::num::NonZeroI32;
use azo_sys::ResultCode;
use crate::{Error, WinResult};
use crate::win::*;

/// Somehow [`CString`] has no equivalent of [`CStr::from_bytes_until_nul`] - <https://github.com/rust-lang/rust/pull/96186>
///
/// Drivers fill fixed-size name buffers (32 bytes for driver, channel and clock source names),
/// and a name that uses every byte leaves no room for the terminating NUL. Such a name is taken
/// whole rather than treated as a driver fault.
#[must_use]
pub fn cstring_from_bytes_until_nul(buffer: &[u8]) -> CString {
    match CStr::from_bytes_until_nul(buffer) {
        Ok(name) => name.to_owned(),
        Err(_) => CString::new(buffer).expect("no interior NUL: `from_bytes_until_nul` found none")
    }
}

pub trait ResultCodeExt {
    /// Cannot use [`From`] / [`Into`] because of the orphan rule
    fn to_result(self) -> crate::Result<()>;
    fn to_result_with<T>(self, ok_fn: impl FnOnce() -> T) -> crate::Result<T>;
}

impl ResultCodeExt for ResultCode {
    fn to_result(self) -> crate::Result<()> {
        match self {
            Self::OK |
            Self::SUCCESS => Ok(()),
            
            bad_code => Err(Error(unsafe { NonZeroI32::new_unchecked(bad_code.0) }))
        }
    }
    
    fn to_result_with<T>(self, ok_fn: impl FnOnce() -> T) -> crate::Result<T> {
        self.to_result().map(|()| ok_fn())
    }
}

type PhantomUnSend = PhantomData<*const ()>;

/// Ensures correct pairing of COM init/uninit
#[derive(Debug, PartialEq, Eq)]
pub struct COMInit(PhantomUnSend);

impl COMInit {
    pub fn new() -> WinResult<Self> {
        // STA because most ASIO drivers do not support MTA
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED as _) }
        .map(|| Self(PhantomUnSend::default()))
    }
}

impl Clone for COMInit {
    fn clone(&self) -> Self {
        // The existence of `self` guarantees that this thread is already STA initialized,
        // so re-initializing simply increments the ref count.
        Self::new().expect("COM STA re-initialization should be infallible")
    }
}

impl Drop for COMInit {
    fn drop(&mut self) {
        unsafe { CoUninitialize(); }
    }
}

#[cfg(test)]
mod tests {
    use super::cstring_from_bytes_until_nul;

    #[test]
    fn names_stop_at_the_first_nul_or_fill_the_buffer() {
        assert_eq!(cstring_from_bytes_until_nul(b"Analog 1\0junk").as_bytes(), b"Analog 1");
        assert_eq!(cstring_from_bytes_until_nul(&[b'x'; 32]).as_bytes(), &[b'x'; 32]);
    }
}
