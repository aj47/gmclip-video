//! Playback output (SPEC §6.3, M2): the system's default output device through the
//! `DefaultOutput` AudioUnit, 48 kHz interleaved stereo f32 (the unit converts to the
//! device's own rate). The render callback runs on CoreAudio's real-time thread and
//! hands each buffer to a Rust closure together with the host time (nanoseconds) at
//! which its first sample leaves the speakers: the callback's output time plus the
//! device's and stream's presentation latency. That is the clock video follows.
//!
//! The closure must not block or allocate (it runs on the audio thread); the caller's
//! side feeds it through a channel.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioOutputUnitStart,
    AudioOutputUnitStop, AudioUnit, AudioUnitGetProperty, AudioUnitInitialize,
    AudioUnitRenderActionFlags, AudioUnitSetProperty, AudioUnitUninitialize,
    kAudioOutputUnitProperty_CurrentDevice, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_SetRenderCallback, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global,
    kAudioUnitScope_Input, kAudioUnitSubType_DefaultOutput, kAudioUnitType_Output,
};
use objc2_core_audio::{
    AudioConvertHostTimeToNanos, AudioGetCurrentHostTime, AudioObjectGetPropertyData,
    AudioObjectPropertyAddress, kAudioDevicePropertyLatency, kAudioDevicePropertyStreams,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioStreamPropertyLatency,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags,
    kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM,
};

use super::cf::check;
use crate::GpuError;

/// The output rate the closure fills.
pub const RATE: u32 = 48_000;

/// Fills one buffer: interleaved stereo f32, and the host time (ns) its first sample is
/// heard.
pub type RenderFn = Box<dyn FnMut(&mut [f32], u64) + Send>;

struct Ctx {
    render: RenderFn,
    /// Read by the callback on every buffer; [`AudioOut::refresh_latency`] updates it
    /// while the unit runs (shared, so that never touches the callback's `Ctx`).
    latency_ns: Arc<AtomicU64>,
}

/// One open output unit.
pub struct AudioOut {
    unit: AudioUnit,
    ctx: *mut Ctx,
    running: bool,
    latency_ns: u64,
    shared_latency: Arc<AtomicU64>,
}

// SAFETY: the unit is a CoreAudio object usable from any thread; `ctx` is only touched
// by the render callback while the unit runs and freed after it is stopped/disposed.
unsafe impl Send for AudioOut {}

impl std::fmt::Debug for AudioOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOut")
            .field("running", &self.running)
            .field("latency_ns", &self.latency_ns)
            .finish_non_exhaustive()
    }
}

/// Now, in host-time nanoseconds (the clock the callback's times are on).
pub fn host_now_ns() -> u64 {
    // SAFETY: plain clock reads.
    unsafe { AudioConvertHostTimeToNanos(AudioGetCurrentHostTime()) }
}

/// The system's default output device right now (0 when there is none). Playback
/// compares it to the one it opened with and reopens on a switch (headphones, AirPods),
/// so the latency it adds is the new device's.
pub fn default_output_device() -> u32 {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut dev: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: valid address and out pointers of the property's size.
    let st = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut dev).cast(),
        )
    };
    if st == 0 { dev } else { 0 }
}

unsafe extern "C-unwind" fn render_cb(
    refcon: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    ts: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: `refcon` is the `Ctx` this unit was given; it outlives the running unit.
    let ctx = unsafe { &mut *refcon.as_ptr().cast::<Ctx>() };
    // SAFETY: CoreAudio passes a valid timestamp for this call.
    let ts = unsafe { ts.as_ref() };
    let host_ns = if ts.mFlags.contains(AudioTimeStampFlags::HostTimeValid) {
        // SAFETY: plain conversion.
        unsafe { AudioConvertHostTimeToNanos(ts.mHostTime) }
    } else {
        host_now_ns()
    };
    if data.is_null() {
        return 0;
    }
    // SAFETY: the unit was given one interleaved buffer format: one buffer of
    // `frames * 2` f32s, valid for this call.
    let list = unsafe { &mut *data };
    if list.mNumberBuffers < 1 {
        return 0;
    }
    let b = &mut list.mBuffers[0];
    let n = (frames as usize * 2).min(b.mDataByteSize as usize / 4);
    if b.mData.is_null() || n == 0 {
        return 0;
    }
    // SAFETY: as above; `n` is within the buffer CoreAudio gave us.
    let out = unsafe { std::slice::from_raw_parts_mut(b.mData.cast::<f32>(), n) };
    let at = host_ns + ctx.latency_ns.load(Ordering::Relaxed);
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (ctx.render)(out, at))).is_err() {
        out.fill(0.0);
    }
    0
}

fn device_latency_frames(unit: AudioUnit) -> u32 {
    let mut dev: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: valid unit; `dev`/`size` are valid out pointers of the property's size.
    let st = unsafe {
        AudioUnitGetProperty(
            unit,
            kAudioOutputUnitProperty_CurrentDevice,
            kAudioUnitScope_Global,
            0,
            NonNull::from(&mut dev).cast(),
            NonNull::from(&mut size),
        )
    };
    if st != 0 || dev == 0 {
        return 0;
    }
    let get = |obj: u32, sel: u32| -> u32 {
        let addr = AudioObjectPropertyAddress {
            mSelector: sel,
            mScope: kAudioObjectPropertyScopeOutput,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut v: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: valid address and out pointers of the property's size.
        let st = unsafe {
            AudioObjectGetPropertyData(
                obj,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut v).cast(),
            )
        };
        if st == 0 { v } else { 0 }
    };
    let mut frames = get(dev, kAudioDevicePropertyLatency);
    // the first output stream's own latency
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreams,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut streams = [0u32; 8];
    let mut size = std::mem::size_of_val(&streams) as u32;
    // SAFETY: valid address; `streams` holds up to 8 ids and `size` says so.
    let st = unsafe {
        AudioObjectGetPropertyData(
            dev,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut streams).cast(),
        )
    };
    if st == 0 && size >= 4 {
        frames += get(streams[0], kAudioStreamPropertyLatency);
    }
    frames
}

fn device_rate(unit: AudioUnit) -> f64 {
    let mut f = AudioStreamBasicDescription {
        mSampleRate: 0.0,
        mFormatID: 0,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 0,
        mBytesPerFrame: 0,
        mChannelsPerFrame: 0,
        mBitsPerChannel: 0,
        mReserved: 0,
    };
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    // SAFETY: valid unit; out pointer of the property's size. Output scope, element 0
    // is the device side of the output unit.
    let st = unsafe {
        AudioUnitGetProperty(
            unit,
            kAudioUnitProperty_StreamFormat,
            2, // kAudioUnitScope_Output
            0,
            NonNull::from(&mut f).cast(),
            NonNull::from(&mut size),
        )
    };
    if st == 0 && f.mSampleRate > 0.0 {
        f.mSampleRate
    } else {
        f64::from(RATE)
    }
}

impl AudioOut {
    /// Opens the default output device (stopped).
    pub fn open(render: RenderFn) -> Result<Self, GpuError> {
        let desc = AudioComponentDescription {
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_DefaultOutput,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        // SAFETY: valid description; a null start component searches from the first.
        let comp = unsafe { AudioComponentFindNext(std::ptr::null_mut(), NonNull::from(&desc)) };
        if comp.is_null() {
            return Err(GpuError::Unsupported("no default audio output".into()));
        }
        let mut unit: AudioUnit = std::ptr::null_mut();
        // SAFETY: `comp` is a valid component; `unit` receives the instance.
        let st = unsafe { AudioComponentInstanceNew(comp, NonNull::from(&mut unit)) };
        check(st, "audio_out_new")?;
        let fmt = AudioStreamBasicDescription {
            mSampleRate: f64::from(RATE),
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
            mBytesPerPacket: 8,
            mFramesPerPacket: 1,
            mBytesPerFrame: 8,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let shared_latency = Arc::new(AtomicU64::new(0));
        let ctx = Box::into_raw(Box::new(Ctx {
            render,
            latency_ns: Arc::clone(&shared_latency),
        }));
        let cb = AURenderCallbackStruct {
            inputProc: Some(render_cb),
            inputProcRefCon: ctx.cast(),
        };
        let mut me = AudioOut {
            unit,
            ctx,
            running: false,
            latency_ns: 0,
            shared_latency,
        };
        // SAFETY: valid unit; property data are the documented structs, sized.
        unsafe {
            check(
                AudioUnitSetProperty(
                    unit,
                    kAudioUnitProperty_StreamFormat,
                    kAudioUnitScope_Input,
                    0,
                    (&raw const fmt).cast(),
                    std::mem::size_of::<AudioStreamBasicDescription>() as u32,
                ),
                "audio_out_format",
            )?;
            check(
                AudioUnitSetProperty(
                    unit,
                    kAudioUnitProperty_SetRenderCallback,
                    kAudioUnitScope_Input,
                    0,
                    (&raw const cb).cast(),
                    std::mem::size_of::<AURenderCallbackStruct>() as u32,
                ),
                "audio_out_callback",
            )?;
            check(AudioUnitInitialize(unit), "audio_out_init")?;
        }
        me.refresh_latency();
        Ok(me)
    }

    /// Reads the device's presentation latency again (the device and its first output
    /// stream) and hands it to the callback, which adds it from its next buffer on. A
    /// Bluetooth device reports a different latency once its link settles.
    pub fn refresh_latency(&mut self) -> u64 {
        let frames = device_latency_frames(self.unit);
        let rate = device_rate(self.unit);
        self.latency_ns = (f64::from(frames) / rate * 1e9) as u64;
        self.shared_latency
            .store(self.latency_ns, Ordering::Relaxed);
        self.latency_ns
    }

    pub fn start(&mut self) -> Result<(), GpuError> {
        if !self.running {
            // SAFETY: valid, initialised unit.
            let st = unsafe { AudioOutputUnitStart(self.unit) };
            check(st, "audio_out_start")?;
            self.running = true;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.running {
            // SAFETY: valid unit. Stop returns after the callback has finished.
            unsafe { AudioOutputUnitStop(self.unit) };
            self.running = false;
        }
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// The device's presentation latency (already added to the callback's times).
    pub fn latency_ns(&self) -> u64 {
        self.latency_ns
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        self.stop();
        // SAFETY: the unit is stopped; dispose it, then free the context it pointed at.
        unsafe {
            AudioUnitUninitialize(self.unit);
            AudioComponentInstanceDispose(self.unit);
            drop(Box::from_raw(self.ctx));
        }
    }
}
