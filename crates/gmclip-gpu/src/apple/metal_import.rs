//! IOSurface `CVPixelBuffer` → two wgpu plane textures, zero-copy (H-01, SPEC §6.2).
//!
//! A `CVMetalTextureCache` per device turns each plane of a bi-planar buffer into an
//! `MTLTexture` (R8Unorm/RG8Unorm, or R16Unorm/RG16Unorm for 10-bit). Each is wrapped by
//! `wgpu_hal::metal::Device::texture_from_raw` with a drop callback that owns the
//! `CVMetalTexture` and the pixel buffer, so both stay alive until wgpu retires the
//! texture — after the last GPU submission that used it, not when Rust drops the handle
//! (implementer finding 15). wgpu has no NV12 format on Metal; the two planes are two
//! textures.

use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, kCVMetalTextureUsage,
};
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTexture, MTLTextureType, MTLTextureUsage};

use super::cf::{Val, check, dict};
use super::pixel_buffer::{PixelBuffer, PlaneFormat};
use crate::{GpuContext, GpuError};

/// What the imported planes will be used for. Sampling is always allowed.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct ImportUsage {
    /// Render into the planes (`MTLTextureUsageRenderTarget`, H-07).
    pub render_target: bool,
    /// Write the planes from a compute shader (`MTLTextureUsageShaderWrite`, H-07 fallback).
    pub storage: bool,
}

impl ImportUsage {
    pub const SAMPLE: ImportUsage = ImportUsage {
        render_target: false,
        storage: false,
    };
    pub const RENDER: ImportUsage = ImportUsage {
        render_target: true,
        storage: false,
    };
    pub const STORAGE: ImportUsage = ImportUsage {
        render_target: false,
        storage: true,
    };
}

/// Both planes of one imported picture. Dropping it is safe at any time: the
/// underlying `CVMetalTexture`s and the pixel buffer live until wgpu retires the
/// textures.
#[derive(Debug)]
pub struct PlaneTextures {
    pub y: wgpu::Texture,
    pub uv: wgpu::Texture,
    pub width: u32,
    pub height: u32,
    pub format: PlaneFormat,
    /// The `MTLTextureUsage` bits the cache actually granted, per plane.
    pub granted_usage: [u64; 2],
}

/// Live [`Keepalive`]s, for the lifetime tests.
static LIVE_KEEPALIVES: AtomicUsize = AtomicUsize::new(0);

/// How many imported planes still hold their `CVMetalTexture` + pixel buffer, i.e. have
/// not yet been retired by wgpu. Diagnostic; used by the H-01 lifetime test.
pub fn live_keepalives() -> usize {
    LIVE_KEEPALIVES.load(Ordering::SeqCst)
}

/// Keeps a plane's `CVMetalTexture` and its pixel buffer alive until wgpu drops the
/// hal texture.
///
/// Ordering matters: wgpu-hal 30's `metal::Texture` declares its drop guard as the last
/// field (`metal/mod.rs` "must be the last field"), so the `MTLTexture` is released
/// before this keepalive runs. Releasing the `CVMetalTexture` first strands the
/// IOSurface (measured by the H-01 review: ~3 MB leaked per cycle). Re-check that
/// field order on every wgpu bump; the lifetime test below catches a regression.
struct Keepalive {
    _cv_texture: CFRetained<CVMetalTexture>,
    _buffer: PixelBuffer,
}

impl Drop for Keepalive {
    fn drop(&mut self) {
        LIVE_KEEPALIVES.fetch_sub(1, Ordering::SeqCst);
    }
}

// SAFETY: CVMetalTexture and CVPixelBuffer are CF objects with atomic retain/release.
// The keepalive is never accessed, only dropped — on whichever thread drops wgpu's last
// reference to the texture (the caller's, or the one polling the device).
unsafe impl Send for Keepalive {}
// SAFETY: as above; the keepalive exposes no methods.
unsafe impl Sync for Keepalive {}

/// The per-device `CVMetalTextureCache`.
pub struct MetalImporter {
    device: wgpu::Device,
    adapter: wgpu::Adapter,
    cache: CFRetained<CVMetalTextureCache>,
    /// Serialises every call into the cache (creation and flush): CoreVideo does not
    /// document `CVMetalTextureCache` as thread-safe.
    lock: Mutex<()>,
    norm16: bool,
}

// SAFETY: the cache is a CF object with atomic retain/release and no thread affinity;
// wgpu::Device / Adapter are Send + Sync.
unsafe impl Send for MetalImporter {}
// SAFETY: every use of the cache through `&self` holds `lock`.
unsafe impl Sync for MetalImporter {}

impl std::fmt::Debug for MetalImporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetalImporter").finish_non_exhaustive()
    }
}

/// The `MTLDevice` behind a wgpu device created on the Metal backend.
pub(crate) fn raw_metal_device(
    device: &wgpu::Device,
) -> Result<Retained<ProtocolObject<dyn MTLDevice>>, GpuError> {
    // SAFETY: we only read the raw device handle and retain it; we never destroy it or
    // use it to break wgpu's invariants (it is used for the texture cache only).
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
        .ok_or_else(|| GpuError::Unsupported("wgpu device is not on the Metal backend".into()))?;
    Ok(hal.raw_device().clone())
}

impl MetalImporter {
    pub fn new(ctx: &GpuContext) -> Result<Self, GpuError> {
        let raw = raw_metal_device(&ctx.device)?;
        let mut out: *mut CVMetalTextureCache = std::ptr::null_mut();
        // SAFETY: `raw` is a live MTLDevice; `out` receives a +1 retained cache.
        let status =
            unsafe { CVMetalTextureCache::create(None, None, &raw, None, NonNull::from(&mut out)) };
        check(status, "CVMetalTextureCacheCreate")?;
        let ptr = NonNull::new(out).ok_or(GpuError::Os {
            step: "CVMetalTextureCacheCreate",
            status: -1,
        })?;
        Ok(Self {
            device: ctx.device.clone(),
            adapter: ctx.adapter.clone(),
            // SAFETY: Create rule, +1 retained.
            cache: unsafe { CFRetained::from_raw(ptr) },
            lock: Mutex::new(()),
            norm16: ctx.norm16,
        })
    }

    /// Wraps both planes of `pb` as wgpu textures.
    pub fn import(&self, pb: &PixelBuffer, usage: ImportUsage) -> Result<PlaneTextures, GpuError> {
        let format = pb
            .format()
            .ok_or_else(|| GpuError::Unsupported(format!("pixel format {:#x}", pb.fourcc())))?;
        if format == PlaneFormat::P010Video && !self.norm16 {
            return Err(GpuError::Unsupported(
                "10-bit planes need TEXTURE_FORMAT_16BIT_NORM".into(),
            ));
        }
        if !pb.is_iosurface_backed() {
            return Err(GpuError::Unsupported(
                "pixel buffer is not IOSurface-backed".into(),
            ));
        }
        let [fy, fuv] = format.plane_formats();
        let (y, gy) = self.import_plane(pb, 0, fy, usage)?;
        let (uv, guv) = self.import_plane(pb, 1, fuv, usage)?;
        Ok(PlaneTextures {
            y,
            uv,
            width: pb.width(),
            height: pb.height(),
            format,
            granted_usage: [gy, guv],
        })
    }

    fn import_plane(
        &self,
        pb: &PixelBuffer,
        plane: usize,
        format: wgpu::TextureFormat,
        usage: ImportUsage,
    ) -> Result<(wgpu::Texture, u64), GpuError> {
        let (w, h) = pb.plane_size(plane);
        let mtl_format = match format {
            wgpu::TextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
            wgpu::TextureFormat::Rg8Unorm => MTLPixelFormat::RG8Unorm,
            wgpu::TextureFormat::R16Unorm => MTLPixelFormat::R16Unorm,
            wgpu::TextureFormat::Rg16Unorm => MTLPixelFormat::RG16Unorm,
            other => return Err(GpuError::Unsupported(format!("plane format {other:?}"))),
        };
        let mut mtl_usage = MTLTextureUsage::ShaderRead;
        let mut wgpu_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC;
        if usage.render_target {
            mtl_usage |= MTLTextureUsage::RenderTarget;
            wgpu_usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        if usage.storage {
            mtl_usage |= MTLTextureUsage::ShaderWrite;
            wgpu_usage |= wgpu::TextureUsages::STORAGE_BINDING;
        }
        // `create_texture_from_hal` does not check usage against the format's features;
        // a texture wgpu cannot use that way would only fail later, at first use.
        let allowed = self
            .adapter
            .get_texture_format_features(format)
            .allowed_usages;
        if !allowed.contains(wgpu_usage) {
            return Err(GpuError::Unsupported(format!(
                "{format:?} planes: usage {wgpu_usage:?} not allowed by the device ({allowed:?})"
            )));
        }
        // SAFETY: immutable CFString exported by CoreVideo.
        let usage_key = unsafe { kCVMetalTextureUsage };
        let attrs = dict(&[(usage_key, Val::I64(mtl_usage.0 as i64))]);

        let guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: *mut CVMetalTexture = std::ptr::null_mut();
        // SAFETY: cache access serialised by `guard`; `pb` is a live IOSurface-backed buffer with plane `plane` of `w × h`;
        // the format matches the plane's sample layout; `out` receives a +1 retained
        // CVMetalTexture.
        let status = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.cache,
                pb.raw(),
                Some(attrs.as_opaque()),
                mtl_format,
                w as usize,
                h as usize,
                plane,
                NonNull::from(&mut out),
            )
        };
        drop(guard);
        check(status, "CVMetalTextureCacheCreateTextureFromImage")?;
        let ptr = NonNull::new(out).ok_or(GpuError::Os {
            step: "CVMetalTextureCacheCreateTextureFromImage",
            status: -1,
        })?;
        // SAFETY: Create rule, +1 retained.
        let cv_texture: CFRetained<CVMetalTexture> = unsafe { CFRetained::from_raw(ptr) };
        // CVMetalTextureGetTexture follows the Get rule (+0). The binding retains it with
        // `retain_autoreleased`, which on objc2 < 0.6.5 had a use-after-free on a +0
        // return (objc2 #861) — hence the 0.6.5 floor. The pool is belt and braces.
        let mtl =
            autoreleasepool(|_| CVMetalTextureGetTexture(&cv_texture)).ok_or(GpuError::Os {
                step: "CVMetalTextureGetTexture",
                status: -1,
            })?;
        debug_assert_eq!((mtl.width(), mtl.height()), (w as usize, h as usize));
        debug_assert_eq!(mtl.pixelFormat(), mtl_format);
        let granted = mtl.usage().0 as u64;
        if granted & mtl_usage.0 as u64 != mtl_usage.0 as u64 {
            return Err(GpuError::Unsupported(format!(
                "texture cache granted usage {granted:#x}, asked {:#x}",
                mtl_usage.0
            )));
        }
        LIVE_KEEPALIVES.fetch_add(1, Ordering::SeqCst);
        let keep = Keepalive {
            _cv_texture: cv_texture,
            _buffer: pb.retain(),
        };
        // SAFETY: `mtl` is a 2D texture of `format` and `w × h × 1` with one mip and one
        // layer (a CVMetalTexture plane). The drop callback owns the CVMetalTexture and
        // the pixel buffer, so the IOSurface memory outlives every GPU use of `mtl`.
        let hal_texture = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                mtl,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width: w,
                    height: h,
                    depth: 1,
                },
                Some(Box::new(move || drop(keep))),
            )
        };
        let desc = wgpu::TextureDescriptor {
            label: Some(if plane == 0 { "plane-y" } else { "plane-uv" }),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu_usage,
            view_formats: &[],
        };
        // SAFETY: the hal texture was created from this device's MTLDevice (the cache
        // was made from it) and respects `desc`: format and size are the plane's, and
        // the usage was both granted by the cache and allowed for the format (checked
        // above). Its contents are whatever the IOSurface holds — defined bytes, which
        // is what wgpu's "initialised" requires (wgpu-core sets no clear for hal
        // textures). Metal has no layouts, so RESOURCE matches the driver state.
        let texture = unsafe {
            self.device
                .create_texture_from_hal::<wgpu::hal::api::Metal>(
                    hal_texture,
                    &desc,
                    wgpu::TextureUses::RESOURCE,
                )
        };
        Ok((texture, granted))
    }

    /// Lets the cache recycle textures whose last reference is gone. Call periodically
    /// (e.g. once per output frame).
    pub fn flush(&self) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.cache.flush(0);
    }
}
