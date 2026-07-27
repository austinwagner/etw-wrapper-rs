//! Provider registration, enablement tracking, and event writing.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::bindings::{
    EVENT_CONTROL_CODE_DISABLE_PROVIDER, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
    EVENT_FILTER_DESCRIPTOR, EventRegister, EventUnregister, EventWriteTransfer, REGHANDLE,
};
use crate::error::win32_result;
use crate::field::EventDataDescriptor;
use crate::{EVENT_DATA_DESCRIPTOR, EVENT_DESCRIPTOR, GUID, Result};

/// A registered ETW provider.
///
/// The provider is unregistered when this is dropped.
pub struct EtwLogger {
    ctx: Box<EtwContext>,
}

impl EtwLogger {
    /// Registers the provider with the given GUID.
    pub fn register(guid: &GUID) -> Result<Self> {
        Ok(EtwLogger {
            ctx: EtwContext::register(guid)?,
        })
    }

    /// Returns whether the provider is enabled for the specified level and keyword.
    #[must_use]
    #[inline]
    pub fn enabled(&self, level: u8, keyword: u64) -> bool {
        self.ctx.enabled(level, keyword)
    }

    /// Writes a single event.
    pub fn write(&self, descriptor: &EVENT_DESCRIPTOR, data: &[EventDataDescriptor]) -> Result<()> {
        self.ctx.write(descriptor, data)
    }
}

/// Tracks provider registration and enablement.
struct EtwContext {
    registration_handle: REGHANDLE,
    is_enabled: AtomicU32,
    level: AtomicU8,
    match_any_keyword: AtomicU64,
    match_all_keyword: AtomicU64,
}

impl EtwContext {
    fn new() -> Self {
        EtwContext {
            registration_handle: 0,
            is_enabled: AtomicU32::new(0),
            level: AtomicU8::new(0),
            match_any_keyword: AtomicU64::new(0),
            match_all_keyword: AtomicU64::new(0),
        }
    }

    /// Registers the provider with the given GUID.
    ///
    /// # Safety
    ///
    /// The context must remain in the returned [`Box`]. Moving its data would cause the ETW
    /// callback to access an invalid pointer.
    fn register(guid: &GUID) -> Result<Box<Self>> {
        let mut ctx = Box::new(Self::new());
        let ptr = ctx.as_ref() as *const Self as *const c_void;
        let mut handle: REGHANDLE = 0;
        // SAFETY: `ptr` points to the boxed context, which remains alive until unregistration.
        let res = unsafe { EventRegister(guid, Some(enable_callback), ptr, &mut handle) };
        win32_result(res)?;
        ctx.registration_handle = handle;
        Ok(ctx)
    }

    /// Returns whether the provider is enabled for the specified level and keyword.
    #[inline]
    fn enabled(&self, level: u8, keyword: u64) -> bool {
        if self.is_enabled.load(Ordering::Relaxed) == 0 {
            return false;
        }
        let cur_level = self.level.load(Ordering::Relaxed);
        let any = self.match_any_keyword.load(Ordering::Relaxed);
        let all = self.match_all_keyword.load(Ordering::Relaxed);
        (level <= cur_level || cur_level == 0)
            && (keyword == 0 || ((keyword & any) != 0 && (keyword & all) == all))
    }

    /// Writes a single event.
    fn write(&self, descriptor: &EVENT_DESCRIPTOR, data: &[EventDataDescriptor]) -> Result<()> {
        // SAFETY: `EventDataDescriptor` transparently wraps `EVENT_DATA_DESCRIPTOR`, and the
        // borrowed payloads outlive this call.
        let res = unsafe {
            EventWriteTransfer(
                self.registration_handle,
                descriptor,
                std::ptr::null(),
                std::ptr::null(),
                data.len() as u32,
                data.as_ptr() as *const EVENT_DATA_DESCRIPTOR,
            )
        };
        win32_result(res)
    }
}

impl Drop for EtwContext {
    fn drop(&mut self) {
        // We have to create a context to get its pointer prior to registering it. If registration
	// fails then we will have a null handle that we don't want to try to unregister.
        if self.registration_handle == 0 {
            return;
        }

        // SAFETY: the handle came from a successful `EventRegister` and is unregistered once,
        // when the sole owning `Box` is dropped.
        unsafe {
            let _ = EventUnregister(self.registration_handle);
        }
    }
}

/// Handles an ETW enable or disable notification.
///
/// Mutates the context only through atomics to avoid undefined behavior. The callback provides no
/// ordering guarantees, but a transient read is acceptable for the lightweight enablement check.
unsafe extern "system" fn enable_callback(
    _source_id: *const GUID,
    is_enabled: u32,
    level: u8,
    match_any_keyword: u64,
    match_all_keyword: u64,
    _filter_data: *const EVENT_FILTER_DESCRIPTOR,
    callback_context: *mut c_void,
) {
    if callback_context.is_null() {
        return;
    }
    let ctx = unsafe { &*(callback_context as *const EtwContext) };

    match is_enabled {
        EVENT_CONTROL_CODE_ENABLE_PROVIDER => {
            ctx.level.store(level, Ordering::Relaxed);
            ctx.match_any_keyword
                .store(match_any_keyword, Ordering::Relaxed);
            ctx.match_all_keyword
                .store(match_all_keyword, Ordering::Relaxed);
            ctx.is_enabled
                .store(EVENT_CONTROL_CODE_ENABLE_PROVIDER, Ordering::Relaxed);
        }
        EVENT_CONTROL_CODE_DISABLE_PROVIDER => {
            ctx.is_enabled
                .store(EVENT_CONTROL_CODE_DISABLE_PROVIDER, Ordering::Relaxed);
            ctx.level.store(0, Ordering::Relaxed);
            ctx.match_any_keyword.store(0, Ordering::Relaxed);
            ctx.match_all_keyword.store(0, Ordering::Relaxed);
        }
        _ => {}
    }
}
