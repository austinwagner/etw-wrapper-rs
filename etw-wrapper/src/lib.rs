// On docs.rs (which sets `--cfg docsrs`) opt into the nightly `doc_cfg` feature so items gated
// behind a Cargo feature render an "Available on crate feature ..." banner in the docs.
#![cfg_attr(docsrs, feature(doc_cfg))]

//! A strongly typed Event Tracing for Windows wrapper.
//!
//! The `gen_etw_wrapper!` macro turns an ETW manifest into a strongly typed provider struct.
//! See the examples directory for a basic demonstration.
//!
//! ```no_run
//! use etw_wrapper::{Result, gen_etw_wrapper};
//!
//! gen_etw_wrapper!("manifests/widgetservice.man", PROVIDER_WIDGETSERVICE -> WidgetLogger);
//!
//! fn main() -> Result<()> {
//!     let provider = WidgetLogger::register()?;
//!     provider.request_failed(0x12ABCDEF, 500, 42, "failed to succeed")?;
//!     Ok(())
//! }
//! ```
//!
//! To avoid the procedural macro, call [`EtwLogger::register`], check [`EtwLogger::enabled`]
//! before each call to [`EtwLogger::write`], and convert parameters to
//! [`EVENT_DATA_DESCRIPTOR`] instances with the helpers in the [`field`] module.
//!
//! ```no_run
//! use etw_wrapper::{EVENT_DESCRIPTOR, EtwLogger, GUID, field::*};
//! let ctx = EtwLogger::register(&GUID::default()).unwrap();
//! let x = 1u32;
//! let y = to_u16cstring("hello");
//!
//! let descriptor = EVENT_DESCRIPTOR::default();
//! ctx.write(&descriptor, &[scalar(&x), str16(&y)]).unwrap();
//! ```

#[cfg(feature = "macro")]
#[cfg_attr(docsrs, doc(cfg(feature = "macro")))]
pub use etw_wrapper_macro::gen_etw_wrapper;

// Gives generated code a stable path when the macro is invoked from this package's own targets.
extern crate self as etw_wrapper;

mod context;
pub mod field;

pub use context::EtwLogger;

// Re-export Windows types so callers do not need a direct windows dependency
pub use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
pub use windows::Win32::Security::PSID;
pub use windows::Win32::System::Diagnostics::Etw::{EVENT_DATA_DESCRIPTOR, EVENT_DESCRIPTOR};
pub use windows::core::{Error, GUID, Result};
