//! Person segmentation with Apple Vision (SPEC §5.8 roto, §9 Segmentation [M4 Mac]):
//! `VNGeneratePersonSegmentationRequest` at `.fast` for the draft matte the preview shows
//! live and `.accurate` for the mask tracks export uses.
//!
//! Vision is an OS framework reached through the Objective-C runtime (`objc2`'s
//! `msg_send!`), so no binding crate is added; the classes are looked up by name and a
//! missing class (an OS without Vision's person segmentation) is an error, never a panic.
//!
//! Input is a crop of a decoded picture copied into its own 8-bit NV12 buffer (the
//! crop's source pixels: SPEC "export resolution: crop at source res"); output is an
//! 8-bit mask at whatever size the model produces (`.fast` ≈ 256 px, `.accurate` up to
//! ≈ 2 K on the long side), which the caller resamples to the crop.

use std::ptr::NonNull;

use objc2::msg_send;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};

use super::pixel_buffer::{PixelBuffer, PlaneFormat};
use crate::GpuError;

#[link(name = "Vision", kind = "framework")]
unsafe extern "C" {}

/// A source-pixel rectangle `(x, y, w, h)`.
pub type PixelRect = (u32, u32, u32, u32);

/// `kCVPixelFormatType_OneComponent8` ('L008').
const ONE_COMPONENT_8: u32 = 0x4C30_3038;

/// Vision's `VNGeneratePersonSegmentationRequestQualityLevel`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SegQuality {
    /// `.accurate` (0): export masks.
    Accurate,
    /// `.balanced` (1).
    Balanced,
    /// `.fast` (2): the live draft.
    Fast,
}

impl SegQuality {
    fn level(self) -> usize {
        match self {
            SegQuality::Accurate => 0,
            SegQuality::Balanced => 1,
            SegQuality::Fast => 2,
        }
    }

    /// The name stored with a mask track (`apple_vision.accurate`, …).
    pub fn model_name(self) -> &'static str {
        match self {
            SegQuality::Accurate => "apple_vision.accurate",
            SegQuality::Balanced => "apple_vision.balanced",
            SegQuality::Fast => "apple_vision.fast",
        }
    }
}

/// An 8-bit single-channel mask, rows of `width` bytes (255 = person).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisionMask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass, GpuError> {
    AnyClass::get(name).ok_or_else(|| {
        GpuError::Unsupported(format!(
            "Apple Vision class {} is not available on this macOS",
            name.to_string_lossy()
        ))
    })
}

/// A person-segmentation session. With [`Segmenter::sequence`] frames go through one
/// `VNSequenceRequestHandler` (Vision may use the previous frames: a track is built in
/// frame order); otherwise every frame is independent (random access: the draft).
#[derive(Debug)]
pub struct Segmenter {
    request: Retained<AnyObject>,
    sequence: Option<Retained<AnyObject>>,
    quality: SegQuality,
}

// SAFETY: the request and handler are only messaged through `&mut self`, so one thread
// uses them at a time; Vision objects are not bound to the thread that made them.
unsafe impl Send for Segmenter {}

impl Segmenter {
    /// A session whose frames are independent.
    pub fn new(quality: SegQuality) -> Result<Self, GpuError> {
        Self::make(quality, false)
    }

    /// A session for consecutive frames of one track.
    pub fn sequence(quality: SegQuality) -> Result<Self, GpuError> {
        Self::make(quality, true)
    }

    fn make(quality: SegQuality, sequence: bool) -> Result<Self, GpuError> {
        let req_cls = class(c"VNGeneratePersonSegmentationRequest")?;
        let request: Retained<AnyObject> = autoreleasepool(|_| {
            // SAFETY: `+new` on an NSObject subclass returns a +1 instance (or nil).
            let r: Option<Retained<AnyObject>> = unsafe { msg_send![req_cls, new] };
            let r = r.ok_or(GpuError::Os {
                step: "VNGeneratePersonSegmentationRequest",
                status: -1,
            })?;
            // SAFETY: `qualityLevel` is an NSUInteger property and `outputPixelFormat`
            // an OSType property of the request (macOS 12+).
            unsafe {
                let _: () = msg_send![&*r, setQualityLevel: quality.level()];
                let _: () = msg_send![&*r, setOutputPixelFormat: ONE_COMPONENT_8];
            }
            Ok::<_, GpuError>(r)
        })?;
        let sequence = if sequence {
            let cls = class(c"VNSequenceRequestHandler")?;
            // SAFETY: `+new` returns a +1 VNSequenceRequestHandler.
            let h: Option<Retained<AnyObject>> = unsafe { msg_send![cls, new] };
            Some(h.ok_or(GpuError::Os {
                step: "VNSequenceRequestHandler",
                status: -1,
            })?)
        } else {
            None
        };
        Ok(Self {
            request,
            sequence,
            quality,
        })
    }

    pub fn quality(&self) -> SegQuality {
        self.quality
    }

    /// Segments the whole of `pb` (8-bit NV12 or BGRA).
    pub fn segment(&mut self, pb: &PixelBuffer) -> Result<VisionMask, GpuError> {
        autoreleasepool(|_| self.segment_in_pool(pb))
    }

    /// Copies the crop `(x, y, w, h)` (source pixels, clamped, snapped to even
    /// coordinates for the 4:2:0 chroma) of `pb` and segments it. Returns the mask and
    /// the rectangle actually segmented.
    pub fn segment_crop(
        &mut self,
        pb: &PixelBuffer,
        rect: PixelRect,
    ) -> Result<(VisionMask, PixelRect), GpuError> {
        let (crop, r) = crop_nv12(pb, rect)?;
        Ok((self.segment(&crop)?, r))
    }

    fn segment_in_pool(&mut self, pb: &PixelBuffer) -> Result<VisionMask, GpuError> {
        let raw: &CVPixelBuffer = pb.raw();
        let nsarray = class(c"NSArray")?;
        // SAFETY: `+arrayWithObject:` returns an autoreleased NSArray holding the request.
        let requests: Option<Retained<AnyObject>> =
            unsafe { msg_send![nsarray, arrayWithObject: &*self.request] };
        let requests = requests.ok_or(GpuError::Os {
            step: "NSArray",
            status: -1,
        })?;
        let mut err: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = match &self.sequence {
            // SAFETY: `performRequests:onCVPixelBuffer:error:` takes an NSArray of
            // requests, a CVPixelBufferRef that outlives the call, and an NSError** out
            // parameter (autoreleased on failure).
            Some(seq) => unsafe {
                msg_send![&**seq, performRequests: &*requests, onCVPixelBuffer: raw, error: &mut err]
            },
            None => {
                let nsdict = class(c"NSDictionary")?;
                // SAFETY: `+dictionary` returns an autoreleased empty NSDictionary.
                let opts: Option<Retained<AnyObject>> = unsafe { msg_send![nsdict, dictionary] };
                let opts = opts.ok_or(GpuError::Os {
                    step: "NSDictionary",
                    status: -1,
                })?;
                let cls = class(c"VNImageRequestHandler")?;
                // SAFETY: `-initWithCVPixelBuffer:options:` on a freshly allocated handler
                // retains the buffer for the handler's lifetime.
                let handler: Option<Retained<AnyObject>> = unsafe {
                    let alloc: objc2::rc::Allocated<AnyObject> = msg_send![cls, alloc];
                    msg_send![alloc, initWithCVPixelBuffer: raw, options: &*opts]
                };
                let handler = handler.ok_or(GpuError::Os {
                    step: "VNImageRequestHandler",
                    status: -1,
                })?;
                // SAFETY: as above; `performRequests:error:` on the image handler.
                unsafe { msg_send![&*handler, performRequests: &*requests, error: &mut err] }
            }
        };
        if !ok {
            let code: isize = if err.is_null() {
                -1
            } else {
                // SAFETY: on failure Vision sets `err` to an autoreleased NSError, alive
                // until the enclosing pool drains.
                unsafe { msg_send![&*err, code] }
            };
            return Err(GpuError::Os {
                step: "VNGeneratePersonSegmentationRequest",
                status: code as i64,
            });
        }
        // SAFETY: `results` is an NSArray of VNPixelBufferObservation (or nil).
        let results: Option<Retained<AnyObject>> = unsafe { msg_send![&*self.request, results] };
        let first: Option<Retained<AnyObject>> = match &results {
            // SAFETY: `-firstObject` on an NSArray.
            Some(a) => unsafe { msg_send![&**a, firstObject] },
            None => None,
        };
        let obs = first.ok_or(GpuError::Os {
            step: "VNPixelBufferObservation",
            status: -1,
        })?;
        // SAFETY: `pixelBuffer` returns the observation's CVPixelBufferRef (+0, owned by
        // the observation, which `obs` keeps alive while it is read below).
        let mask: *mut CVPixelBuffer = unsafe { msg_send![&*obs, pixelBuffer] };
        let mask = NonNull::new(mask).ok_or(GpuError::Os {
            step: "VNPixelBufferObservation.pixelBuffer",
            status: -1,
        })?;
        // SAFETY: `mask` is valid while `obs` lives (this scope).
        let mask: &CVPixelBuffer = unsafe { mask.as_ref() };
        read_one_component(mask)
    }
}

/// Copies an 8-bit single-channel buffer out.
fn read_one_component(pb: &CVPixelBuffer) -> Result<VisionMask, GpuError> {
    if CVPixelBufferGetPixelFormatType(pb) != ONE_COMPONENT_8 {
        return Err(GpuError::Unsupported(format!(
            "Vision mask format {:#x}",
            CVPixelBufferGetPixelFormatType(pb)
        )));
    }
    let (w, h) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
    // SAFETY: a read-only lock on a valid buffer, balanced below.
    let status = unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) };
    super::cf::check(status, "CVPixelBufferLockBaseAddress")?;
    let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
    let stride = CVPixelBufferGetBytesPerRow(pb);
    let mut data = vec![0u8; w * h];
    let ok = !base.is_null() && stride >= w;
    if ok {
        for r in 0..h {
            // SAFETY: the buffer is locked; row `r` of `stride ≥ w` bytes lies inside it,
            // and `data` is a distinct Rust allocation.
            unsafe {
                std::ptr::copy_nonoverlapping(base.add(r * stride), data[r * w..].as_mut_ptr(), w)
            };
        }
    }
    // SAFETY: balances the lock above.
    unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) };
    if !ok {
        return Err(GpuError::Unsupported("Vision mask: bad layout".into()));
    }
    Ok(VisionMask {
        width: w as u32,
        height: h as u32,
        data,
    })
}

/// The crop rectangle snapped to even coordinates and clamped to `(w, h)`.
pub fn snap_crop(rect: (u32, u32, u32, u32), w: u32, h: u32) -> (u32, u32, u32, u32) {
    let x0 = (rect.0.min(w.saturating_sub(2))) & !1;
    let y0 = (rect.1.min(h.saturating_sub(2))) & !1;
    let x1 = (rect.0.saturating_add(rect.2)).min(w).max(x0 + 2);
    let y1 = (rect.1.saturating_add(rect.3)).min(h).max(y0 + 2);
    let cw = ((x1 - x0) + 1) & !1;
    let ch = ((y1 - y0) + 1) & !1;
    (x0, y0, cw.min(w - x0), ch.min(h - y0))
}

/// Copies `rect` of an NV12 / P010 picture into a new 8-bit NV12 buffer (P010 keeps its
/// high byte).
pub fn crop_nv12(pb: &PixelBuffer, rect: PixelRect) -> Result<(PixelBuffer, PixelRect), GpuError> {
    let fmt = pb
        .format()
        .ok_or_else(|| GpuError::Unsupported(format!("pixel format {:#x}", pb.fourcc())))?;
    let (sw, sh) = (pb.width(), pb.height());
    if sw < 2 || sh < 2 {
        return Err(GpuError::Unsupported("picture smaller than 2×2".into()));
    }
    let r = snap_crop(rect, sw, sh);
    let (x0, y0, w, h) = r;
    let out_fmt = match fmt {
        PlaneFormat::Nv12Full => PlaneFormat::Nv12Full,
        _ => PlaneFormat::Nv12Video,
    };
    let out = PixelBuffer::new(w, h, out_fmt)?;
    let bps = fmt.bytes_per_sample();
    let src = pb.raw();
    let dst = out.raw();
    // SAFETY: read-only lock on the source and a write lock on our new buffer, both
    // balanced at the end of this function; nothing else holds `out` yet.
    unsafe {
        super::cf::check(
            CVPixelBufferLockBaseAddress(src, CVPixelBufferLockFlags::ReadOnly),
            "CVPixelBufferLockBaseAddress",
        )?;
        let s = CVPixelBufferLockBaseAddress(dst, CVPixelBufferLockFlags::empty());
        if s != 0 {
            CVPixelBufferUnlockBaseAddress(src, CVPixelBufferLockFlags::ReadOnly);
            super::cf::check(s, "CVPixelBufferLockBaseAddress")?;
        }
    }
    let mut result = Ok(());
    for plane in 0..2usize {
        let (px0, py0, pw, ph, comps) = if plane == 0 {
            (x0, y0, w, h, 1usize)
        } else {
            (x0 / 2, y0 / 2, w / 2, h / 2, 2usize)
        };
        let sb = CVPixelBufferGetBaseAddressOfPlane(src, plane) as *const u8;
        let ss = CVPixelBufferGetBytesPerRowOfPlane(src, plane);
        let db = CVPixelBufferGetBaseAddressOfPlane(dst, plane) as *mut u8;
        let ds = CVPixelBufferGetBytesPerRowOfPlane(dst, plane);
        let (spw, sph) = pb.plane_size(plane);
        let row = pw as usize * comps;
        if sb.is_null()
            || db.is_null()
            || ds < row
            || px0 + pw > spw
            || py0 + ph > sph
            || ss < (spw as usize) * comps * bps
        {
            result = Err(GpuError::Unsupported(format!(
                "crop plane {plane}: bad layout"
            )));
            break;
        }
        let mut tmp = vec![0u8; row];
        for y in 0..ph as usize {
            let off = (py0 as usize + y) * ss + px0 as usize * comps * bps;
            if bps == 1 {
                // SAFETY: both buffers are locked; the source row segment
                // `[off, off + row)` and destination row `[y·ds, y·ds + row)` lie inside
                // their planes (checked above); `tmp` is Rust-owned.
                unsafe {
                    std::ptr::copy_nonoverlapping(sb.add(off), tmp.as_mut_ptr(), row);
                }
            } else {
                for (i, t) in tmp.iter_mut().enumerate() {
                    // SAFETY: as above; the high byte of the little-endian 16-bit sample
                    // `i` lies inside the locked source row.
                    *t = unsafe { *sb.add(off + i * 2 + 1) };
                }
            }
            // SAFETY: see the copy above.
            unsafe {
                std::ptr::copy_nonoverlapping(tmp.as_ptr(), db.add(y * ds), row);
            }
        }
    }
    // SAFETY: balances the two locks taken above.
    unsafe {
        CVPixelBufferUnlockBaseAddress(dst, CVPixelBufferLockFlags::empty());
        CVPixelBufferUnlockBaseAddress(src, CVPixelBufferLockFlags::ReadOnly);
    }
    result?;
    Ok((out, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_is_even_and_inside() {
        assert_eq!(snap_crop((3, 5, 101, 51), 640, 360), (2, 4, 102, 52));
        assert_eq!(
            snap_crop((600, 300, 100, 100), 640, 360),
            (600, 300, 40, 60)
        );
        let r = snap_crop((639, 359, 9, 9), 640, 360);
        assert!(r.0.is_multiple_of(2) && r.1.is_multiple_of(2) && r.2 >= 2 && r.3 >= 2);
        assert!(r.0 + r.2 <= 640 && r.1 + r.3 <= 360);
    }
}
