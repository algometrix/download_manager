//! COM on the calling thread, for the taskbar (single-threaded apartment,
//! on the UI thread) and notifications (multithreaded, on their own thread).

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
};

/// Which kind of apartment a thread joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apartment {
    SingleThreaded,
    MultiThreaded,
}

#[derive(Debug, thiserror::Error)]
pub enum ComError {
    #[error("this thread already uses COM in the other threading model")]
    OtherThreadingModel,
    #[error("COM did not start: {0}")]
    Start(windows::core::Error),
}

/// Joins this thread to a COM apartment and leaves it when dropped. COM
/// objects must be released before this guard drops, so callers declare it
/// after (and so drop it after) the objects they create.
pub struct ComApartment {
    /// COM state is per thread; the guard must drop on the thread that
    /// entered.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComApartment {
    pub fn enter(apartment: Apartment) -> Result<ComApartment, ComError> {
        let model: COINIT = match apartment {
            Apartment::SingleThreaded => COINIT_APARTMENTTHREADED,
            Apartment::MultiThreaded => COINIT_MULTITHREADED,
        };
        // SAFETY: initializes COM for this thread only. Success includes
        // S_FALSE (already initialized), which still needs the matching
        // `CoUninitialize` in `drop`.
        let result = unsafe { CoInitializeEx(None, model) };
        if result == RPC_E_CHANGED_MODE {
            return Err(ComError::OtherThreadingModel);
        }
        result.ok().map_err(ComError::Start)?;
        Ok(ComApartment {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful `CoInitializeEx` in `enter`, on
        // the same thread (the guard is not `Send`).
        unsafe { CoUninitialize() };
    }
}
