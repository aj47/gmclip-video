//! wgpu device bootstrap (H-01).
//!
//! macOS uses the Metal backend only; elsewhere wgpu picks its primary backend. The
//! device asks for `TEXTURE_FORMAT_16BIT_NORM` when the adapter has it, which is what
//! 10-bit `x420` planes need (R16Unorm / Rg16Unorm, implementer finding 15).

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Errors from device bootstrap and the OS imports in this crate.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter: {0}")]
    NoAdapter(String),
    #[error("device request failed: {0}")]
    Device(String),
    /// An OS call returned a non-zero status (`OSStatus` / `CVReturn`).
    #[error("{step}: OS status {status}")]
    Os { step: &'static str, status: i64 },
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A bounded pixel-buffer pool has every buffer out (`kCVReturnWouldExceedAllocationThreshold`):
    /// back-pressure, the caller must release frames first.
    #[error("pixel buffer pool exhausted ({0} buffers out)")]
    PoolExhausted(u32),
}

impl GpuError {
    /// The OS status, when the error came from an OS call.
    pub fn os_status(&self) -> Option<i64> {
        match self {
            GpuError::Os { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// Options for [`GpuContext::new`].
#[derive(Clone, Debug, Default)]
pub struct GpuOptions {
    /// wgpu's own validation (debug builds only; Metal API validation is the
    /// `MTL_DEBUG_LAYER=1` environment variable, set before the process starts).
    pub validation: bool,
}

/// The wgpu instance, adapter, device and queue every pass shares.
#[derive(Debug)]
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Whether R16Unorm / Rg16Unorm textures are available (10-bit planes).
    pub norm16: bool,
}

impl GpuContext {
    /// Creates the device. On macOS this is always the Metal backend.
    pub fn new(opts: &GpuOptions) -> Result<Self, GpuError> {
        let backends = if cfg!(target_vendor = "apple") {
            wgpu::Backends::METAL
        } else {
            // WGPU_BACKEND=dx12|vulkan picks one (the Windows GPU checks run both). On
            // Windows DX12 by default: the native API, the one Media Foundation's D3D11
            // pictures share with, and quicker to start (cold: 0.31 s against Vulkan's
            // 0.55 s on the RTX 5080, which the primary set picked)
            wgpu::Backends::from_env().unwrap_or(if cfg!(windows) {
                wgpu::Backends::DX12
            } else {
                wgpu::Backends::PRIMARY
            })
        };
        // wgpu's defaults for the build (validation in debug builds), overridable with
        // WGPU_* environment variables.
        let mut flags = wgpu::InstanceFlags::from_build_config().with_env();
        if opts.validation {
            flags |= wgpu::InstanceFlags::VALIDATION | wgpu::InstanceFlags::DEBUG;
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .map_err(|e| GpuError::NoAdapter(e.to_string()))?;
        let norm16 = adapter
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        // 16-bit norm planes (10-bit video), and the adapter's real per-format
        // capabilities (render-target / storage planes for the NV12 output, H-07).
        let wanted = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let required_features = adapter.features() & wanted;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gmclip"),
            required_features,
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| GpuError::Device(e.to_string()))?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            norm16,
        })
    }

    /// Blocks until all submitted GPU work has completed.
    pub fn wait_idle(&self) -> Result<(), GpuError> {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map(|_| ())
            .map_err(|e| GpuError::Device(e.to_string()))
    }
}

/// Polls a future to completion on this thread. wgpu's native futures resolve without
/// an executor, so a no-op waker and a spin with `yield_now` is enough.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}
