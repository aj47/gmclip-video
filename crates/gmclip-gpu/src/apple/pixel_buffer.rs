//! IOSurface-backed `CVPixelBuffer`s: the frames VideoToolbox decodes into and encodes
//! from, and the planes the compositor samples and renders into (H-01, H-07).

use std::ptr::NonNull;

use objc2_core_foundation::{CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetIOSurface, CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount,
    CVPixelBufferGetWidth, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelBufferPoolAllocationThresholdKey, kCVPixelBufferPoolMinimumBufferCountKey,
    kCVPixelBufferWidthKey, kCVReturnWouldExceedAllocationThreshold,
};

use super::cf::{Val, check, dict};
use crate::GpuError;

/// Bi-planar 4:2:0 layouts gmclip reads and writes.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum PlaneFormat {
    /// `420v`: 8-bit, video (limited) range.
    Nv12Video,
    /// `420f`: 8-bit, full range.
    Nv12Full,
    /// `x420`: 10-bit in the high bits of 16-bit samples, video range.
    P010Video,
}

impl PlaneFormat {
    /// The CoreVideo pixel format four-char code.
    pub const fn fourcc(self) -> u32 {
        match self {
            PlaneFormat::Nv12Video => 0x3432_3076, // '420v'
            PlaneFormat::Nv12Full => 0x3432_3066,  // '420f'
            PlaneFormat::P010Video => 0x7834_3230, // 'x420'
        }
    }

    pub const fn from_fourcc(code: u32) -> Option<Self> {
        match code {
            0x3432_3076 => Some(PlaneFormat::Nv12Video),
            0x3432_3066 => Some(PlaneFormat::Nv12Full),
            0x7834_3230 => Some(PlaneFormat::P010Video),
            _ => None,
        }
    }

    /// Bytes per sample in each plane (1 for 8-bit, 2 for 10-bit).
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            PlaneFormat::Nv12Video | PlaneFormat::Nv12Full => 1,
            PlaneFormat::P010Video => 2,
        }
    }

    /// The wgpu formats of the luma and chroma planes.
    pub const fn plane_formats(self) -> [wgpu::TextureFormat; 2] {
        match self {
            PlaneFormat::Nv12Video | PlaneFormat::Nv12Full => {
                [wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm]
            }
            PlaneFormat::P010Video => [
                wgpu::TextureFormat::R16Unorm,
                wgpu::TextureFormat::Rg16Unorm,
            ],
        }
    }
}

/// Tightly packed copies of the two planes of a 4:2:0 bi-planar picture: luma rows of
/// `width · bps` bytes, then interleaved CbCr rows of `ceil(width/2) · 2 · bps` bytes.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct CpuPlanes {
    pub width: u32,
    pub height: u32,
    pub format: PlaneFormat,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

impl CpuPlanes {
    pub fn chroma_size(width: u32, height: u32) -> (u32, u32) {
        (width.div_ceil(2), height.div_ceil(2))
    }

    /// A zeroed picture.
    pub fn new(width: u32, height: u32, format: PlaneFormat) -> Self {
        let bps = format.bytes_per_sample();
        let (cw, ch) = Self::chroma_size(width, height);
        Self {
            width,
            height,
            format,
            y: vec![0; width as usize * height as usize * bps],
            uv: vec![0; cw as usize * ch as usize * 2 * bps],
        }
    }

    /// Planar U then V (8-bit only): the layout ffmpeg's `yuv420p` raw output uses.
    pub fn uv_planar(&self) -> Vec<u8> {
        assert_eq!(self.format.bytes_per_sample(), 1, "uv_planar is 8-bit only");
        let n = self.uv.len() / 2;
        let mut out = vec![0u8; self.uv.len()];
        for i in 0..n {
            out[i] = self.uv[2 * i];
            out[n + i] = self.uv[2 * i + 1];
        }
        out
    }
}

/// A retained `CVPixelBuffer`.
///
/// Not `Clone`: CPU writes take `&mut self`, so two Rust handles can never write (or
/// write and read) the same pixels at once. Other references to the same CF object
/// exist outside Rust's view — the decoder that produced it, a texture-import
/// keepalive, the encoder — and the GPU or the OS may touch the memory through them;
/// CPU copies therefore go through raw pointers (`ptr::copy_nonoverlapping`) and never
/// form a Rust reference into IOSurface memory, so such a race tears pixels but is not
/// undefined behaviour. Callers order GPU work before CPU reads (wait for the submit).
pub struct PixelBuffer {
    raw: CFRetained<CVPixelBuffer>,
}

// SAFETY: a CVPixelBuffer is a CoreFoundation object: retain and release are atomic and
// its metadata getters only read immutable properties. Pixel memory is accessed only
// by raw-pointer copies between Lock/Unlock (see the type docs), never through Rust
// references, so moving the handle across threads cannot create aliasing references.
unsafe impl Send for PixelBuffer {}
// SAFETY: `&PixelBuffer` permits only metadata reads and raw-pointer copies *out of*
// the planes (`read_planes`); writes need `&mut PixelBuffer`.
unsafe impl Sync for PixelBuffer {}

impl std::fmt::Debug for PixelBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBuffer")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("fourcc", &format_args!("{:#010x}", self.fourcc()))
            .finish()
    }
}

/// The attributes every gmclip pixel buffer gets: IOSurface-backed and Metal-compatible.
fn attributes(
    width: u32,
    height: u32,
    format: PlaneFormat,
) -> CFRetained<objc2_core_foundation::CFDictionary<CFString, CFType>> {
    // SAFETY: the `kCV…Key` statics are immutable CFStrings exported by CoreVideo.
    let (fmt_key, w_key, h_key, ios_key, mtl_key) = unsafe {
        (
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferWidthKey,
            kCVPixelBufferHeightKey,
            kCVPixelBufferIOSurfacePropertiesKey,
            kCVPixelBufferMetalCompatibilityKey,
        )
    };
    dict(&[
        (fmt_key, Val::I64(format.fourcc() as i64)),
        (w_key, Val::I64(width as i64)),
        (h_key, Val::I64(height as i64)),
        (ios_key, Val::EmptyDict),
        (mtl_key, Val::Bool(true)),
    ])
}

impl PixelBuffer {
    /// Wraps a retained buffer handed out by VideoToolbox or a pool.
    pub(crate) fn from_retained(raw: CFRetained<CVPixelBuffer>) -> Self {
        Self { raw }
    }

    pub(crate) fn raw(&self) -> &CVPixelBuffer {
        &self.raw
    }

    /// Another retained handle to the same buffer, for keepalives and OS hand-offs
    /// inside this crate (never handed out: see the type docs).
    pub(crate) fn retain(&self) -> Self {
        Self {
            raw: self.raw.clone(),
        }
    }

    /// A new IOSurface-backed, Metal-compatible buffer.
    pub fn new(width: u32, height: u32, format: PlaneFormat) -> Result<Self, GpuError> {
        let attrs = attributes(width, height, format);
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: `attrs` is a valid CFDictionary of CFString keys; `out` is a valid
        // out-pointer that CoreVideo writes a +1 retained buffer into on success.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width as usize,
                height as usize,
                format.fourcc(),
                Some(attrs.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        check(status, "CVPixelBufferCreate")?;
        let ptr = NonNull::new(out).ok_or(GpuError::Os {
            step: "CVPixelBufferCreate",
            status: -1,
        })?;
        // SAFETY: CVPixelBufferCreate follows the Create rule: `ptr` is +1 retained.
        Ok(Self::from_retained(unsafe { CFRetained::from_raw(ptr) }))
    }

    pub fn width(&self) -> u32 {
        CVPixelBufferGetWidth(&self.raw) as u32
    }

    pub fn height(&self) -> u32 {
        CVPixelBufferGetHeight(&self.raw) as u32
    }

    pub fn fourcc(&self) -> u32 {
        CVPixelBufferGetPixelFormatType(&self.raw)
    }

    pub fn format(&self) -> Option<PlaneFormat> {
        PlaneFormat::from_fourcc(self.fourcc())
    }

    pub fn plane_count(&self) -> usize {
        CVPixelBufferGetPlaneCount(&self.raw)
    }

    /// `(width, height)` of plane `i` in samples.
    pub fn plane_size(&self, i: usize) -> (u32, u32) {
        (
            CVPixelBufferGetWidthOfPlane(&self.raw, i) as u32,
            CVPixelBufferGetHeightOfPlane(&self.raw, i) as u32,
        )
    }

    /// Whether the buffer is backed by an IOSurface (needed for the Metal import).
    pub fn is_iosurface_backed(&self) -> bool {
        CVPixelBufferGetIOSurface(Some(&self.raw)).is_some()
    }

    fn lock(&self, read_only: bool) -> Result<LockGuard<'_>, GpuError> {
        let flags = if read_only {
            CVPixelBufferLockFlags::ReadOnly
        } else {
            CVPixelBufferLockFlags::empty()
        };
        // SAFETY: locking a valid pixel buffer; the guard unlocks with the same flags.
        let status = unsafe { CVPixelBufferLockBaseAddress(&self.raw, flags) };
        check(status, "CVPixelBufferLockBaseAddress")?;
        Ok(LockGuard { pb: self, flags })
    }

    /// Copies both planes out, cropped to the buffer's width and height.
    pub fn read_planes(&self) -> Result<CpuPlanes, GpuError> {
        let format = self
            .format()
            .ok_or_else(|| GpuError::Unsupported(format!("pixel format {:#x}", self.fourcc())))?;
        let (w, h) = (self.width(), self.height());
        let mut out = CpuPlanes::new(w, h, format);
        let bps = format.bytes_per_sample();
        let (cw, ch) = CpuPlanes::chroma_size(w, h);
        let _g = self.lock(true)?;
        for (plane, dst, row_bytes, rows) in [
            (0usize, &mut out.y, w as usize * bps, h as usize),
            (1usize, &mut out.uv, cw as usize * 2 * bps, ch as usize),
        ] {
            let base = CVPixelBufferGetBaseAddressOfPlane(&self.raw, plane) as *const u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&self.raw, plane);
            let (pw, ph) = self.plane_size(plane);
            let plane_row = pw as usize * if plane == 0 { bps } else { 2 * bps };
            if base.is_null() || stride < row_bytes || plane_row < row_bytes || (ph as usize) < rows
            {
                return Err(GpuError::Unsupported(format!(
                    "plane {plane}: stride {stride}, {pw}x{ph}, need {row_bytes} x {rows}"
                )));
            }
            for r in 0..rows {
                let d = &mut dst[r * row_bytes..(r + 1) * row_bytes];
                // SAFETY: the buffer is locked; `base + r·stride` starts row `r` of a plane
                // with at least `rows` rows of `stride ≥ row_bytes` bytes (checked above),
                // and `d` is a distinct Rust allocation of exactly `row_bytes`. No Rust
                // reference into the IOSurface is formed (see the type docs).
                unsafe {
                    std::ptr::copy_nonoverlapping(base.add(r * stride), d.as_mut_ptr(), row_bytes)
                };
            }
        }
        Ok(out)
    }

    /// Copies both planes in. `planes` must match the buffer's size and format. Wait for
    /// any GPU work that reads or writes this buffer first.
    pub fn write_planes(&mut self, planes: &CpuPlanes) -> Result<(), GpuError> {
        if Some(planes.format) != self.format()
            || planes.width != self.width()
            || planes.height != self.height()
        {
            return Err(GpuError::Unsupported(
                "write_planes: size or format mismatch".into(),
            ));
        }
        let bps = planes.format.bytes_per_sample();
        let (w, h) = (planes.width as usize, planes.height as usize);
        let (cw, ch) = CpuPlanes::chroma_size(planes.width, planes.height);
        let _g = self.lock(false)?;
        for (plane, src, row_bytes, rows) in [
            (0usize, &planes.y, w * bps, h),
            (1usize, &planes.uv, cw as usize * 2 * bps, ch as usize),
        ] {
            let base = CVPixelBufferGetBaseAddressOfPlane(&self.raw, plane) as *mut u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&self.raw, plane);
            let (_, ph) = self.plane_size(plane);
            if base.is_null()
                || stride < row_bytes
                || (ph as usize) < rows
                || src.len() < rows * row_bytes
            {
                return Err(GpuError::Unsupported(format!("plane {plane}: bad layout")));
            }
            for r in 0..rows {
                let s = &src[r * row_bytes..(r + 1) * row_bytes];
                // SAFETY: the buffer is locked for writing; row `r` lies inside the plane
                // (checked above) and does not overlap `s`, which is Rust-owned memory.
                unsafe {
                    std::ptr::copy_nonoverlapping(s.as_ptr(), base.add(r * stride), row_bytes)
                };
            }
        }
        Ok(())
    }
}

struct LockGuard<'a> {
    pb: &'a PixelBuffer,
    flags: CVPixelBufferLockFlags,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: balances the successful lock taken in `PixelBuffer::lock`.
        unsafe {
            CVPixelBufferUnlockBaseAddress(&self.pb.raw, self.flags);
        }
    }
}

/// A `CVPixelBufferPool` of IOSurface-backed, Metal-compatible buffers.
pub struct PixelBufferPool {
    raw: CFRetained<CVPixelBufferPool>,
    /// Serialises buffer creation: CoreVideo does not document the pool as thread-safe.
    lock: std::sync::Mutex<()>,
    /// At most this many buffers out at once (`None`: unbounded).
    threshold: Option<u32>,
    width: u32,
    height: u32,
    format: PlaneFormat,
}

// SAFETY: a CF object with atomic retain/release; nothing in it is tied to the thread
// that created it.
unsafe impl Send for PixelBufferPool {}
// SAFETY: the only operation through `&self` that touches the pool (`get`) holds
// `lock`, so calls are serialised whatever CoreVideo's own guarantees are.
unsafe impl Sync for PixelBufferPool {}

impl std::fmt::Debug for PixelBufferPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBufferPool")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .finish()
    }
}

impl PixelBufferPool {
    pub fn new(
        width: u32,
        height: u32,
        format: PlaneFormat,
        min_buffers: u32,
    ) -> Result<Self, GpuError> {
        let attrs = attributes(width, height, format);
        // SAFETY: immutable CFString exported by CoreVideo.
        let min_key = unsafe { kCVPixelBufferPoolMinimumBufferCountKey };
        let pool_attrs = dict(&[(min_key, Val::I64(min_buffers as i64))]);
        let mut out: *mut CVPixelBufferPool = std::ptr::null_mut();
        // SAFETY: both dictionaries are valid; `out` receives a +1 retained pool.
        let status = unsafe {
            CVPixelBufferPool::create(
                None,
                Some(pool_attrs.as_opaque()),
                Some(attrs.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        check(status, "CVPixelBufferPoolCreate")?;
        let ptr = NonNull::new(out).ok_or(GpuError::Os {
            step: "CVPixelBufferPoolCreate",
            status: -1,
        })?;
        Ok(Self {
            // SAFETY: Create rule, +1 retained.
            raw: unsafe { CFRetained::from_raw(ptr) },
            lock: std::sync::Mutex::new(()),
            threshold: None,
            width,
            height,
            format,
        })
    }

    /// Caps the buffers out at once: [`Self::get`] then fails with
    /// [`GpuError::PoolExhausted`] instead of allocating more.
    pub fn with_threshold(mut self, max_buffers: u32) -> Self {
        self.threshold = Some(max_buffers.max(1));
        self
    }

    /// A buffer from the pool (recycled once every holder drops it).
    pub fn get(&self) -> Result<PixelBuffer, GpuError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        let status = match self.threshold {
            None => {
                // SAFETY: valid pool, creation serialised by `lock`; `out` receives a
                // +1 retained buffer.
                unsafe {
                    CVPixelBufferPool::create_pixel_buffer(None, &self.raw, NonNull::from(&mut out))
                }
            }
            Some(max) => {
                // SAFETY: immutable CFString exported by CoreVideo.
                let key = unsafe { kCVPixelBufferPoolAllocationThresholdKey };
                let aux = dict(&[(key, Val::I64(i64::from(max)))]);
                // SAFETY: as above, plus a valid aux-attributes dictionary.
                let status = unsafe {
                    CVPixelBufferPool::create_pixel_buffer_with_aux_attributes(
                        None,
                        &self.raw,
                        Some(aux.as_opaque()),
                        NonNull::from(&mut out),
                    )
                };
                if status == kCVReturnWouldExceedAllocationThreshold {
                    return Err(GpuError::PoolExhausted(max));
                }
                status
            }
        };
        check(status, "CVPixelBufferPoolCreatePixelBuffer")?;
        let ptr = NonNull::new(out).ok_or(GpuError::Os {
            step: "CVPixelBufferPoolCreatePixelBuffer",
            status: -1,
        })?;
        // SAFETY: Create rule, +1 retained.
        Ok(PixelBuffer::from_retained(unsafe {
            CFRetained::from_raw(ptr)
        }))
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn format(&self) -> PlaneFormat {
        self.format
    }
}
