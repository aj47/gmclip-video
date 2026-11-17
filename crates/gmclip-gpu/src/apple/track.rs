//! Object tracking with Apple Vision (decision 0031): `VNTrackObjectRequest` through one
//! `VNSequenceRequestHandler`, reached through the Objective-C runtime as
//! [`super::vision`] is (no binding crate). A box is followed frame to frame; each frame
//! gives its new box and Vision's confidence.
//!
//! Boxes are normalised to the picture, origin top left (Vision's is bottom left; this
//! module converts both ways).

use objc2::msg_send;
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_video::CVPixelBuffer;

use super::pixel_buffer::PixelBuffer;
use crate::GpuError;

/// A normalised box `(x, y, w, h)`, origin top left.
pub type NormBox = [f64; 4];

fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass, GpuError> {
    AnyClass::get(name).ok_or_else(|| {
        GpuError::Unsupported(format!(
            "Apple Vision class {} is not available on this macOS",
            name.to_string_lossy()
        ))
    })
}

fn to_vision(b: NormBox) -> CGRect {
    CGRect::new(
        CGPoint::new(b[0], 1.0 - b[1] - b[3]),
        CGSize::new(b[2], b[3]),
    )
}

fn from_vision(r: CGRect) -> NormBox {
    [
        r.origin.x,
        1.0 - r.origin.y - r.size.height,
        r.size.width,
        r.size.height,
    ]
}

/// A tracking session: [`Tracker::new`] with the box on the first frame, then
/// [`Tracker::next`] on each following frame in order.
#[derive(Debug)]
pub struct Tracker {
    handler: Retained<AnyObject>,
    request: Retained<AnyObject>,
}

// SAFETY: the handler and request are only messaged through `&mut self`, so one thread
// uses them at a time; Vision objects are not bound to the thread that made them.
unsafe impl Send for Tracker {}

impl Tracker {
    /// Starts following `start` (the box on the first frame), `accurate` tracking.
    pub fn new(start: NormBox) -> Result<Self, GpuError> {
        if !start.iter().all(|v| v.is_finite()) || start[2] <= 0.0 || start[3] <= 0.0 {
            return Err(GpuError::Unsupported("an empty box to track".into()));
        }
        autoreleasepool(|_| {
            let obs_cls = class(c"VNDetectedObjectObservation")?;
            // SAFETY: `+observationWithBoundingBox:` returns an autoreleased observation
            // for a normalised CGRect (passed by value).
            let obs: Option<Retained<AnyObject>> =
                unsafe { msg_send![obs_cls, observationWithBoundingBox: to_vision(start)] };
            let obs = obs.ok_or(GpuError::Os {
                step: "VNDetectedObjectObservation",
                status: -1,
            })?;
            let req_cls = class(c"VNTrackObjectRequest")?;
            // SAFETY: `-initWithDetectedObjectObservation:` on a freshly allocated request
            // retains the observation; `trackingLevel` is an NSUInteger property
            // (VNRequestTrackingLevelAccurate = 0, Fast = 1).
            let request: Option<Retained<AnyObject>> = unsafe {
                let a: Allocated<AnyObject> = msg_send![req_cls, alloc];
                msg_send![a, initWithDetectedObjectObservation: &*obs]
            };
            let request = request.ok_or(GpuError::Os {
                step: "VNTrackObjectRequest",
                status: -1,
            })?;
            // SAFETY: see above.
            unsafe {
                let _: () = msg_send![&*request, setTrackingLevel: 0usize];
            }
            let h_cls = class(c"VNSequenceRequestHandler")?;
            // SAFETY: `+new` returns a +1 VNSequenceRequestHandler.
            let handler: Option<Retained<AnyObject>> = unsafe { msg_send![h_cls, new] };
            let handler = handler.ok_or(GpuError::Os {
                step: "VNSequenceRequestHandler",
                status: -1,
            })?;
            Ok(Self { handler, request })
        })
    }

    /// The box on the next frame `pb` and Vision's confidence (0–1).
    pub fn next(&mut self, pb: &PixelBuffer) -> Result<(NormBox, f32), GpuError> {
        autoreleasepool(|_| {
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
            // SAFETY: `performRequests:onCVPixelBuffer:error:` takes an NSArray of
            // requests, a CVPixelBufferRef that outlives the call and an NSError** out
            // parameter (autoreleased on failure).
            let ok: bool = unsafe {
                msg_send![&*self.handler, performRequests: &*requests, onCVPixelBuffer: raw, error: &mut err]
            };
            if !ok {
                let code: isize = if err.is_null() {
                    -1
                } else {
                    // SAFETY: on failure Vision sets `err` to an autoreleased NSError.
                    unsafe { msg_send![&*err, code] }
                };
                return Err(GpuError::Os {
                    step: "VNTrackObjectRequest",
                    status: code as i64,
                });
            }
            // SAFETY: `results` is an NSArray of VNDetectedObjectObservation (or nil).
            let results: Option<Retained<AnyObject>> =
                unsafe { msg_send![&*self.request, results] };
            let first: Option<Retained<AnyObject>> = match &results {
                // SAFETY: `-firstObject` on an NSArray.
                Some(a) => unsafe { msg_send![&**a, firstObject] },
                None => None,
            };
            let obs = first.ok_or(GpuError::Os {
                step: "VNDetectedObjectObservation",
                status: -1,
            })?;
            // SAFETY: `boundingBox` returns a CGRect by value; `confidence` a float.
            let (bb, conf): (CGRect, f32) =
                unsafe { (msg_send![&*obs, boundingBox], msg_send![&*obs, confidence]) };
            // the next frame continues from this observation
            // SAFETY: `inputObservation` is a VNDetectedObjectObservation property.
            unsafe {
                let _: () = msg_send![&*self.request, setInputObservation: &*obs];
            }
            Ok((from_vision(bb), conf))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxes_flip_between_origins() {
        let b = [0.1, 0.2, 0.3, 0.4];
        let r = to_vision(b);
        assert!((r.origin.y - 0.4).abs() < 1e-12);
        let back = from_vision(r);
        assert!(b.iter().zip(back).all(|(a, c)| (a - c).abs() < 1e-12));
    }
}
