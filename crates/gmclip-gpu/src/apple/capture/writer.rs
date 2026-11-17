//! One recorded file: an `AVAssetWriter` (QuickTime movie, fragmented every second so
//! a crash loses at most the last second) with a real-time video input and an optional
//! AAC audio input. The session starts at the recording's `t0`, so file time 0 is host
//! time `t0` in every file of a session.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};

use super::{
    CaptureError, class, cmtime, error_text, file_url, host_cmtime, ns_dict, ns_number_f64,
    ns_number_i64, sample_host_time,
};

unsafe extern "C" {
    static AVFileTypeQuickTimeMovie: *const AnyObject;
    static AVMediaTypeVideo: *const AnyObject;
    static AVMediaTypeAudio: *const AnyObject;
    static AVVideoCodecKey: *const AnyObject;
    static AVVideoCodecTypeHEVC: *const AnyObject;
    static AVVideoCodecTypeH264: *const AnyObject;
    static AVVideoWidthKey: *const AnyObject;
    static AVVideoHeightKey: *const AnyObject;
    static AVVideoCompressionPropertiesKey: *const AnyObject;
    static AVVideoAverageBitRateKey: *const AnyObject;
    static AVVideoExpectedSourceFrameRateKey: *const AnyObject;
    static AVVideoMaxKeyFrameIntervalKey: *const AnyObject;
    static AVFormatIDKey: *const AnyObject;
    static AVSampleRateKey: *const AnyObject;
    static AVNumberOfChannelsKey: *const AnyObject;
    static AVEncoderBitRateKey: *const AnyObject;
}

unsafe extern "C" {
    fn CMSampleBufferCreateCopyWithNewTiming(
        alloc: *const c_void,
        original: *mut c_void,
        count: isize,
        timing: *const SampleTiming,
        out: *mut *mut c_void,
    ) -> i32;
    fn CFRelease(cf: *const c_void);
}

/// `CMSampleTimingInfo`.
#[repr(C)]
struct SampleTiming {
    duration: objc2_core_media::CMTime,
    pts: objc2_core_media::CMTime,
    dts: objc2_core_media::CMTime,
}

/// `kAudioFormatMPEG4AAC` ('aac ').
const AAC: i64 = 0x6161_6320;
/// `AVAssetWriterStatus`.
const STATUS_WRITING: isize = 1;
const STATUS_COMPLETED: isize = 2;

/// What a finished file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriterStats {
    pub path: PathBuf,
    pub video_frames: u64,
    pub audio_buffers: u64,
    /// Samples the encoder was not ready for (it fell behind real time).
    pub dropped: u64,
    /// Host time of the first and last video frame written.
    pub first_video: Option<u64>,
    pub last_video: Option<u64>,
    pub width: u32,
    pub height: u32,
}

/// The video track's encoding.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct VideoSettings {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
    pub hevc: bool,
}

pub(crate) struct Writer {
    w: Retained<AnyObject>,
    video: Option<Retained<AnyObject>>,
    audio: Option<Retained<AnyObject>>,
    t0: u64,
    started: bool,
    failed: Option<String>,
    stats: WriterStats,
}

// SAFETY: AVAssetWriter and its inputs may be used from any thread as long as calls are
// not concurrent; every `Writer` lives behind a `Mutex` and is only reached through it.
unsafe impl Send for Writer {}

fn os(step: &'static str, message: impl Into<String>) -> CaptureError {
    CaptureError::Os {
        step,
        message: message.into(),
    }
}

/// Bits per second for a video track: about 0.06 bit per pixel per frame for screen
/// content (sharp text), within 6–80 Mb/s.
fn video_bitrate(s: VideoSettings) -> i64 {
    let b = f64::from(s.w) * f64::from(s.h) * f64::from(s.fps.max(1)) * 0.06;
    b.clamp(6e6, 80e6) as i64
}

impl Writer {
    pub(crate) fn new(path: &Path) -> Result<Self, CaptureError> {
        if path.exists() {
            return Err(os(
                "AVAssetWriter",
                format!("{} already exists", path.display()),
            ));
        }
        let url = file_url(path)?;
        let c = class("AVAssetWriter")?;
        let mut err: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: a file URL, an AVFileType constant and an NSError out-pointer.
        let w: Option<Retained<AnyObject>> = unsafe {
            msg_send![c, assetWriterWithURL: &*url, fileType: AVFileTypeQuickTimeMovie, error: &mut err]
        };
        let w = w.ok_or_else(|| os("AVAssetWriter", error_text(err)))?;
        // SAFETY: -setMovieFragmentInterval: takes a CMTime; one second.
        let () = unsafe { msg_send![&*w, setMovieFragmentInterval: cmtime(1, 1)] };
        Ok(Self {
            w,
            video: None,
            audio: None,
            t0: 0,
            started: false,
            failed: None,
            stats: WriterStats {
                path: path.to_path_buf(),
                ..WriterStats::default()
            },
        })
    }

    fn add_input(
        &mut self,
        media: *const AnyObject,
        settings: &AnyObject,
    ) -> Result<Retained<AnyObject>, CaptureError> {
        let c = class("AVAssetWriterInput")?;
        // SAFETY: an AVMediaType constant and an output-settings dictionary.
        let input: Option<Retained<AnyObject>> =
            unsafe { msg_send![c, assetWriterInputWithMediaType: media, outputSettings: settings] };
        let input = input.ok_or_else(|| os("AVAssetWriterInput", "refused the settings"))?;
        // SAFETY: plain setters and checks on a writer that has not started.
        unsafe {
            let () = msg_send![&*input, setExpectsMediaDataInRealTime: Bool::YES];
            let ok: Bool = msg_send![&*self.w, canAddInput: &*input];
            if !ok.as_bool() {
                return Err(os(
                    "AVAssetWriter addInput",
                    "the writer cannot take this track",
                ));
            }
            let () = msg_send![&*self.w, addInput: &*input];
        }
        Ok(input)
    }

    pub(crate) fn add_video(&mut self, s: VideoSettings) -> Result<(), CaptureError> {
        // SAFETY: AVFoundation's NSString constants, valid for the process.
        let (bitrate, rate, keyint, codec_key, width, height, comp_key, media, codec) = unsafe {
            (
                &*AVVideoAverageBitRateKey,
                &*AVVideoExpectedSourceFrameRateKey,
                &*AVVideoMaxKeyFrameIntervalKey,
                &*AVVideoCodecKey,
                &*AVVideoWidthKey,
                &*AVVideoHeightKey,
                &*AVVideoCompressionPropertiesKey,
                AVMediaTypeVideo,
                &*(if s.hevc {
                    AVVideoCodecTypeHEVC
                } else {
                    AVVideoCodecTypeH264
                }),
            )
        };
        let comp = ns_dict(&[
            (bitrate, &*ns_number_i64(video_bitrate(s))?),
            (rate, &*ns_number_i64(i64::from(s.fps))?),
            (keyint, &*ns_number_i64(i64::from(s.fps) * 2)?),
        ])?;
        let settings = ns_dict(&[
            (codec_key, codec),
            (width, &*ns_number_i64(i64::from(s.w))?),
            (height, &*ns_number_i64(i64::from(s.h))?),
            (comp_key, &*comp),
        ])?;
        let input = self.add_input(media, &settings)?;
        self.video = Some(input);
        self.stats.width = s.w;
        self.stats.height = s.h;
        Ok(())
    }

    pub(crate) fn add_audio(
        &mut self,
        sample_rate: f64,
        channels: u32,
    ) -> Result<(), CaptureError> {
        let channels = channels.clamp(1, 2);
        // SAFETY: AVFoundation's NSString constants, valid for the process.
        let (format, rate, count, bitrate, media) = unsafe {
            (
                &*AVFormatIDKey,
                &*AVSampleRateKey,
                &*AVNumberOfChannelsKey,
                &*AVEncoderBitRateKey,
                AVMediaTypeAudio,
            )
        };
        let settings = ns_dict(&[
            (format, &*ns_number_i64(AAC)?),
            (rate, &*ns_number_f64(sample_rate)?),
            (count, &*ns_number_i64(i64::from(channels))?),
            (bitrate, &*ns_number_i64(96_000 * i64::from(channels))?),
        ])?;
        let input = self.add_input(media, &settings)?;
        self.audio = Some(input);
        Ok(())
    }

    pub(crate) fn has_video(&self) -> bool {
        self.video.is_some()
    }

    pub(crate) fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    /// Starts writing with file time 0 at host time `t0`.
    pub(crate) fn start(&mut self, t0: u64) -> Result<(), CaptureError> {
        // SAFETY: -startWriting after every input was added; the session's start time
        // is a host-clock CMTime.
        let ok: Bool = unsafe { msg_send![&*self.w, startWriting] };
        if !ok.as_bool() {
            // SAFETY: -error is nil or the NSError that stopped the writer.
            let e: *mut AnyObject = unsafe { msg_send![&*self.w, error] };
            return Err(os("AVAssetWriter startWriting", error_text(e)));
        }
        // SAFETY: see above.
        let () = unsafe { msg_send![&*self.w, startSessionAtSourceTime: host_cmtime(t0)] };
        self.t0 = t0;
        self.started = true;
        Ok(())
    }

    pub(crate) fn started(&self) -> bool {
        self.started
    }

    /// Appends one captured sample (video or audio); samples from before `t0`, or that
    /// the encoder is not ready for, are left out.
    pub(crate) fn append(&mut self, video: bool, sbuf: *mut c_void) {
        if !self.started || self.failed.is_some() {
            return;
        }
        let t = sample_host_time(sbuf);
        if t < self.t0 {
            return;
        }
        let Some(input) = (if video {
            self.video.as_ref()
        } else {
            self.audio.as_ref()
        }) else {
            return;
        };
        // The encoder may lag a burst (the screen's grid fills several slots at once):
        // wait up to 20 ms for it before leaving the sample out.
        let mut ready = false;
        for _ in 0..20 {
            // SAFETY: a live input of a writer that is writing.
            let r: Bool = unsafe { msg_send![&**input, isReadyForMoreMediaData] };
            if r.as_bool() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if !ready {
            self.stats.dropped += 1;
            return;
        }
        // SAFETY: `sbuf` is the live CMSampleBuffer handed to the capture callback.
        let sb: &objc2_core_media::CMSampleBuffer = unsafe { &*sbuf.cast() };
        // SAFETY: the input retains what it keeps.
        let ok: Bool = unsafe { msg_send![&**input, appendSampleBuffer: sb] };
        if !ok.as_bool() {
            // SAFETY: -status and -error on the writer.
            let status: isize = unsafe { msg_send![&*self.w, status] };
            if status != STATUS_WRITING {
                // SAFETY: -error is nil or the NSError that stopped the writer.
                let e: *mut AnyObject = unsafe { msg_send![&*self.w, error] };
                self.failed = Some(error_text(e));
            }
            self.stats.dropped += 1;
            return;
        }
        if video {
            self.stats.video_frames += 1;
            self.stats.first_video.get_or_insert(t);
            self.stats.last_video = Some(t);
        } else {
            self.stats.audio_buffers += 1;
        }
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.stats.dropped
    }

    pub(crate) fn t0(&self) -> u64 {
        self.t0
    }

    /// Appends a video sample re-stamped at host time `pts` for `dur` (the screen's
    /// constant-rate grid). False when it was not written.
    pub(crate) fn append_retimed(&mut self, sbuf: *mut c_void, pts: u64, dur: u64) -> bool {
        if !self.started || self.failed.is_some() {
            return false;
        }
        let timing = SampleTiming {
            duration: host_cmtime(dur),
            pts: host_cmtime(pts),
            // SAFETY: zeroed is kCMTimeInvalid (no flags).
            dts: unsafe { std::mem::zeroed() },
        };
        let mut copy: *mut c_void = std::ptr::null_mut();
        // SAFETY: a live sample buffer, one timing entry and an out-pointer; the copy
        // is +1 and released below.
        let status = unsafe {
            CMSampleBufferCreateCopyWithNewTiming(std::ptr::null(), sbuf, 1, &timing, &mut copy)
        };
        if status != 0 || copy.is_null() {
            self.stats.dropped += 1;
            return false;
        }
        let before = self.stats.video_frames;
        self.append(true, copy);
        // SAFETY: releases the copy made above (the input retained what it keeps).
        unsafe { CFRelease(copy) };
        self.stats.video_frames > before
    }

    /// Ends the file and waits for it to be written.
    pub(crate) fn finish(self) -> Result<WriterStats, CaptureError> {
        if let Some(m) = self.failed {
            return Err(os("AVAssetWriter", m));
        }
        if !self.started {
            return Err(os("AVAssetWriter", "no frame arrived after the start"));
        }
        // SAFETY: marks each input finished, then ends the session at the last frame.
        unsafe {
            if let Some(v) = &self.video {
                let () = msg_send![&**v, markAsFinished];
            }
            if let Some(a) = &self.audio {
                let () = msg_send![&**a, markAsFinished];
            }
            if let Some(last) = self.stats.last_video {
                let () = msg_send![&*self.w, endSessionAtSourceTime: host_cmtime(last)];
            }
        }
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move || {
            let _ = tx.send(());
        });
        // SAFETY: a block the writer copies and calls once the file is closed.
        let () = unsafe { msg_send![&*self.w, finishWritingWithCompletionHandler: &*block] };
        super::wait(&rx, "the file to close", 60)?;
        // SAFETY: -status and -error on a finished writer.
        let status: isize = unsafe { msg_send![&*self.w, status] };
        if status != STATUS_COMPLETED {
            // SAFETY: -error is nil or the NSError that stopped the writer.
            let e: *mut AnyObject = unsafe { msg_send![&*self.w, error] };
            return Err(os("AVAssetWriter finishWriting", error_text(e)));
        }
        Ok(self.stats)
    }
}

unsafe extern "C" {
    static AVAssetExportPresetPassthrough: *const AnyObject;
}

/// `AVAssetExportSessionStatusCompleted`.
const EXPORT_COMPLETED: isize = 3;

/// Rewrites a recorded (fragmented) movie as a plain one, copying every track as is
/// (`AVAssetExportPresetPassthrough`, no re-encode): fragments keep a crash from
/// losing more than a second while recording, and the editor's demuxer reads plain
/// movies. The original is replaced only once the copy is complete.
pub fn finalize(path: &Path) -> Result<(), CaptureError> {
    let tmp = path.with_extension("finalizing.mov");
    let _ = std::fs::remove_file(&tmp);
    let (src, dst) = (file_url(path)?, file_url(&tmp)?);
    let asset_class = class("AVURLAsset")?;
    let export_class = class("AVAssetExportSession")?;
    // SAFETY: +URLAssetWithURL:options: with a file URL; +exportSessionWithAsset:
    // presetName: with the passthrough preset constant; plain setters.
    let session: Retained<AnyObject> = unsafe {
        let asset: Option<Retained<AnyObject>> =
            msg_send![asset_class, URLAssetWithURL: &*src, options: std::ptr::null::<AnyObject>()];
        let asset = asset.ok_or_else(|| os("AVURLAsset", "cannot open the recording"))?;
        let s: Option<Retained<AnyObject>> = msg_send![export_class, exportSessionWithAsset: &*asset, presetName: AVAssetExportPresetPassthrough];
        let s = s.ok_or_else(|| os("AVAssetExportSession", "passthrough is not available"))?;
        let () = msg_send![&*s, setOutputURL: &*dst];
        let () = msg_send![&*s, setOutputFileType: AVFileTypeQuickTimeMovie];
        let () = msg_send![&*s, setShouldOptimizeForNetworkUse: Bool::NO];
        s
    };
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move || {
        let _ = tx.send(());
    });
    // SAFETY: a block the session copies and calls once the export ends.
    let () = unsafe { msg_send![&*session, exportAsynchronouslyWithCompletionHandler: &*block] };
    super::wait(&rx, "the recording to be finalized", 3600)?;
    // SAFETY: -status and -error on a finished export session.
    let status: isize = unsafe { msg_send![&*session, status] };
    if status != EXPORT_COMPLETED {
        // SAFETY: as above.
        let e: *mut AnyObject = unsafe { msg_send![&*session, error] };
        let _ = std::fs::remove_file(&tmp);
        return Err(os("AVAssetExportSession", error_text(e)));
    }
    std::fs::rename(&tmp, path).map_err(|e| os("finalize", e.to_string()))
}
