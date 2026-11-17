//! GPU bootstrap (SPEC §3.1): the wgpu device and the hal imports that wrap OS decoder
//! surfaces as textures (IOSurface → Metal on macOS, H-01; D3D12 on Windows, M5), plus
//! the OS media sessions whose FFI needs `unsafe` (VideoToolbox, AudioToolbox).
//!
//! This is the **only** gmclip crate allowed to contain `unsafe` (`cargo xt deps`
//! checks every other crate root forbids it). Every `unsafe` block here carries a
//! `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod device;

#[cfg(feature = "alloc-probe")]
pub mod alloc_probe;

#[cfg(target_os = "macos")]
pub mod apple;

#[cfg(windows)]
pub mod windows;

pub use device::{GpuContext, GpuError, GpuOptions, block_on};
