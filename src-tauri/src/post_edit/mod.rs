//! Platform-independent pieces of post-injection observation.
//!
//! No observer is enabled by this module alone. Native workers must supply
//! snapshots from the same control and respect the consent and lifecycle gates.

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod coordinator;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod diagnostics;
pub mod diff;
pub mod observation;
pub mod platform;
pub mod range;
pub mod records;
