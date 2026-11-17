//! Platform services (SPEC §2, §3.11). Stubs at W0, plus the Windows FFI the local IPC
//! and the private files need (decisions/0019).
//!
//! `unsafe` here is Windows-only: off Windows the root forbids it, exactly as in every
//! other crate but gmclip-gpu. On Windows only [`windows`] may use it, and every block
//! carries a `// SAFETY:` comment (`cargo xt deps` checks the root attributes).
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod keychain {}
pub mod fonts {}
/// Codec and GPU capability probe (R-PLAT-1).
pub mod caps {}
pub mod display_link {}
/// Thread QoS at spawn (`QOS_CLASS_UTILITY`, EcoQoS).
pub mod qos {}

pub mod sddl;
pub mod windialog;
pub mod winpath;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;
