/// System audio capture via CATapDescription (macOS 14.2+).
///
/// Uses AudioHardwareCreateProcessTap to create a global mono tap capturing
/// all system audio output, then resamples to the requested rate via
/// AudioConverter. The IOProc callback writes PCM-16 into a crossbeam channel.
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::thread;

// AnyThread provides alloc() for ObjC classes
use objc2::AnyThread;

use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
    AudioDeviceStart, AudioDeviceStop,
    AudioHardwareDestroyAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress,
    CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceIsAlive,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, kAudioTapPropertyUID,
};

// AudioTimeStamp and AudioBufferList come from objc2_core_audio_types
// (matching the types used in AudioDeviceIOProc / AudioDeviceIOProcID)
use objc2_core_audio_types::{AudioTimeStamp, AudioBufferList as ObjcABL};

use coreaudio_sys::{
    AudioBuffer, AudioBufferList,
    AudioConverterDispose, AudioConverterFillComplexBuffer,
    AudioConverterNew, AudioConverterRef,
    AudioStreamBasicDescription, OSStatus,
    kAudioFormatFlagIsPacked,
    kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM,
};

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation::boolean::CFBoolean;
use core_foundation::array::CFArray;

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSNumber, NSString, NSUUID};

use crossbeam_channel::{bounded, Sender};

use crate::{AudioChunk, Error, Recording, Result};

// ── AudioHardwareCreateAggregateDevice rebound with CFDictionaryRef ───────────
// objc2_core_audio's binding takes &objc2_core_foundation::CFDictionary, which
// is a different Rust type from core_foundation::CFDictionary.  We rebind with
// the raw C pointer type to avoid needing objc2_core_foundation in our Cargo.toml.
#[link(name = "CoreAudio", kind = "framework")]
extern "C-unwind" {
    #[link_name = "AudioHardwareCreateAggregateDevice"]
    fn create_aggregate_device(
        desc: core_foundation_sys::dictionary::CFDictionaryRef,
        out_id: *mut AudioObjectID,
    ) -> OSStatus;
}

// ── Context passed to the IOProc ──────────────────────────────────────────────

struct TapContext {
    tx:               Sender<AudioChunk>,
    stopping:         Arc<AtomicBool>,
    converter:        AudioConverterRef,   // null if no conversion needed
    target_rate:      f64,
    source_channels:  u32,
    source_bpf:       u32,   // bytes per frame of source
    // Pre-allocated conversion input staging (IOProc fills, converter reads)
    conv_input_ptr:   *mut c_void,
    conv_input_frames: u32,
    conv_input_bytes:  u32,
}

unsafe impl Send for TapContext {}
unsafe impl Sync for TapContext {}


// ── AudioConverter complex-input callback ─────────────────────────────────────
// Must be `extern "C"` (not "C-unwind") to match AudioConverterComplexInputDataProc.

unsafe extern "C" fn converter_input_proc(
    _converter: AudioConverterRef,
    io_packets: *mut u32,
    io_data: *mut AudioBufferList,
    _out_desc: *mut *mut coreaudio_sys::AudioStreamPacketDescription,
    client_data: *mut c_void,
) -> OSStatus {
    let ctx = &mut *(client_data as *mut TapContext);
    if ctx.conv_input_ptr.is_null() || ctx.conv_input_frames == 0 {
        *io_packets = 0;
        (*io_data).mBuffers[0].mData = std::ptr::null_mut();
        (*io_data).mBuffers[0].mDataByteSize = 0;
        return 0;
    }
    *io_packets = ctx.conv_input_frames;
    (*io_data).mBuffers[0].mData = ctx.conv_input_ptr;
    (*io_data).mBuffers[0].mDataByteSize = ctx.conv_input_bytes;
    // Signal consumed
    ctx.conv_input_ptr = std::ptr::null_mut();
    ctx.conv_input_frames = 0;
    ctx.conv_input_bytes = 0;
    0
}

// ── IOProc ────────────────────────────────────────────────────────────────────
// Signature must exactly match AudioDeviceIOProc (from objc2_core_audio):
//   unsafe extern "C-unwind" fn(AudioObjectID,
//       NonNull<AudioTimeStamp>, NonNull<ObjcABL>,
//       NonNull<AudioTimeStamp>, NonNull<ObjcABL>,
//       NonNull<AudioTimeStamp>, *mut c_void) -> OSStatus

unsafe extern "C-unwind" fn audio_io_proc(
    _device:      AudioObjectID,
    _now:         NonNull<AudioTimeStamp>,
    input_data:   NonNull<ObjcABL>,
    _input_time:  NonNull<AudioTimeStamp>,
    _output_data: NonNull<ObjcABL>,
    _output_time: NonNull<AudioTimeStamp>,
    client_data:  *mut c_void,
) -> OSStatus {
    let ctx = &mut *(client_data as *mut TapContext);
    if ctx.stopping.load(Ordering::Relaxed) {
        return 0;
    }

    let bl = input_data.as_ref();
    if bl.mNumberBuffers == 0 { return 0; }
    let src_buf = &bl.mBuffers[0];
    if src_buf.mData.is_null() || src_buf.mDataByteSize == 0 { return 0; }

    if ctx.converter.is_null() {
        // No conversion needed — data is already PCM-16 mono at target rate.
        let n_frames = src_buf.mDataByteSize as usize / 2;
        let samples = std::slice::from_raw_parts(src_buf.mData as *const i16, n_frames);
        let chunk = AudioChunk { pcm: samples.to_vec() };
        let _ = ctx.tx.try_send(chunk); // drop if full — never block on audio thread
        return 0;
    }

    // Stage input for the converter callback
    ctx.conv_input_ptr    = src_buf.mData;
    ctx.conv_input_frames = src_buf.mDataByteSize / ctx.source_bpf.max(1);
    ctx.conv_input_bytes  = src_buf.mDataByteSize;
    if ctx.conv_input_frames == 0 { return 0; }

    // Estimate output frame count
    let out_frames = ((ctx.conv_input_frames as f64 * ctx.target_rate
        / (ctx.source_bpf as f64 / (ctx.source_channels as f64 * 4.0))
        .max(1.0)) as u32 + 64).max(64);
    let out_bytes = out_frames * 2; // PCM-16 mono

    let mut out_buf = vec![0i16; out_frames as usize];

    // Construct a coreaudio_sys::AudioBufferList for the converter output
    let mut out_bl = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: out_bytes,
            mData: out_buf.as_mut_ptr() as *mut c_void,
        }],
    };

    let mut actual = out_frames;
    let status = AudioConverterFillComplexBuffer(
        ctx.converter,
        Some(converter_input_proc),
        ctx as *mut _ as *mut c_void,
        &mut actual,
        &mut out_bl,
        std::ptr::null_mut(),
    );

    if status == 0 && actual > 0 {
        out_buf.truncate(actual as usize);
        let chunk = AudioChunk { pcm: out_buf };
        let _ = ctx.tx.try_send(chunk);
    }

    0
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn macos_version() -> (u32, u32) {
    use std::ffi::CStr;
    let mut buf = [0u8; 64];
    let name = c"kern.osproductversion";
    let mut sz = buf.len();
    unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr() as *mut c_void, &mut sz, std::ptr::null_mut(), 0) };
    let s = CStr::from_bytes_until_nul(&buf).unwrap_or_default().to_str().unwrap_or_default();
    let mut parts = s.splitn(2, '.');
    let maj: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let min: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    (maj, min)
}

/// Get the UID string for a process tap object.
fn get_tap_uid(tap_id: AudioObjectID) -> Option<String> {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioTapPropertyUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut cf_ref: core_foundation_sys::string::CFStringRef = std::ptr::null();
    let mut size = std::mem::size_of::<core_foundation_sys::string::CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap_id,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(&mut cf_ref as *mut _ as *mut c_void),
        )
    };
    if status != 0 || cf_ref.is_null() { return None; }
    let s = unsafe { CFString::wrap_under_create_rule(cf_ref) };
    Some(s.to_string())
}

/// Get the AudioStreamBasicDescription for a process tap object.
fn get_tap_format(tap_id: AudioObjectID) -> Option<AudioStreamBasicDescription> {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioTapPropertyFormat,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut asbd: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap_id,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(&mut asbd as *mut _ as *mut c_void),
        )
    };
    if status != 0 { None } else { Some(asbd) }
}

/// Poll until the aggregate device reports alive (up to 2 seconds).
fn wait_for_device(device_id: AudioObjectID) -> bool {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyDeviceIsAlive,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    for _ in 0..20 {
        let mut alive: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device_id,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new_unchecked(&mut alive as *mut _ as *mut c_void),
            )
        };
        if status == 0 && alive != 0 { return true; }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

// ── Public entry point ────────────────────────────────────────────────────────

pub(crate) fn start(sample_rate: u32, _chunk_ms: u32) -> Result<Recording> {
    let (maj, min) = macos_version();
    if maj < 14 || (maj == 14 && min < 2) {
        return Err(Error::MacOSVersionTooOld { major: maj, minor: min });
    }

    let (tx, rx) = bounded::<AudioChunk>(64);
    let stopping = Arc::new(AtomicBool::new(false));

    // ── 1. Create CATapDescription ─────────────────────────────────────────
    // initMonoGlobalTapButExcludeProcesses captures all system output as mono.
    let desc: Retained<CATapDescription> = unsafe {
        let empty: Retained<NSArray<NSNumber>> = NSArray::new();
        CATapDescription::initMonoGlobalTapButExcludeProcesses(
            CATapDescription::alloc(),  // AnyThread::alloc
            &empty,
        )
    };
    unsafe {
        let name = NSString::from_str("side-huddle-tap");
        desc.setName(&name);
        let uuid = NSUUID::new();
        desc.setUUID(&uuid);
        desc.setPrivate(true);                        // was: setPrivateTap (wrong name)
        desc.setMuteBehavior(CATapMuteBehavior::Unmuted); // was: ::new(0) (not a fn)
    }

    // ── 2. Create process tap ──────────────────────────────────────────────
    let mut tap_id: AudioObjectID = 0;
    let status = unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id) };
    if status != 0 {
        return Err(Error::RecordingFailed(format!("AudioHardwareCreateProcessTap: {status}")));
    }

    // ── 3. Get tap UID and format ──────────────────────────────────────────
    let tap_uid = get_tap_uid(tap_id)
        .ok_or_else(|| Error::RecordingFailed("failed to get tap UID".into()))?;

    let tap_fmt = get_tap_format(tap_id)
        .ok_or_else(|| Error::RecordingFailed("failed to get tap format".into()))?;

    let source_rate     = if tap_fmt.mSampleRate > 0.0 { tap_fmt.mSampleRate } else { 48000.0 };
    let source_channels = if tap_fmt.mChannelsPerFrame > 0 { tap_fmt.mChannelsPerFrame } else { 2 };
    let source_bpf      = if tap_fmt.mBytesPerFrame > 0 { tap_fmt.mBytesPerFrame } else { source_channels * 4 };

    // ── 4. Create aggregate device wrapping the tap ────────────────────────
    // A unique UID is required; we use the uuid crate which is in Cargo.toml.
    let uid_str = format!("com.side-huddle.agg.{}", uuid::Uuid::new_v4());

    // The tap sub-tap entry dictionary: { "uid": "<tap_uid>" }
    let tap_entry: CFDictionary<CFString, CFString> = {
        // kAudioSubTapUIDKey is a &'static CStr — convert to &str for CFString::new
        let k = CFString::new(kAudioSubTapUIDKey.to_str().unwrap_or("uid"));
        let v = CFString::new(&tap_uid);
        CFDictionary::from_CFType_pairs(&[(k, v)])
    };

    // Build the aggregate device description.
    // All values are wrapped as CFType so from_CFType_pairs<CFString, CFType> works.
    let tap_list = CFArray::from_CFTypes(&[tap_entry.as_CFType()]);
    let entries: Vec<(CFString, CFType)> = vec![
        (CFString::new(kAudioAggregateDeviceNameKey.to_str().unwrap_or("name")),
            CFString::new("MeetingListenerAgg").as_CFType()),
        (CFString::new(kAudioAggregateDeviceUIDKey.to_str().unwrap_or("uid")),
            CFString::new(&uid_str).as_CFType()),
        (CFString::new(kAudioAggregateDeviceSubDeviceListKey.to_str().unwrap_or("subdevices")),
            CFArray::<CFString>::from_CFTypes(&[]).as_CFType()),
        (CFString::new(kAudioAggregateDeviceTapListKey.to_str().unwrap_or("taps")),
            tap_list.as_CFType()),
        (CFString::new(kAudioAggregateDeviceTapAutoStartKey.to_str().unwrap_or("tapAutoStart")),
            CFBoolean::false_value().as_CFType()),
        (CFString::new(kAudioAggregateDeviceIsPrivateKey.to_str().unwrap_or("private")),
            CFBoolean::true_value().as_CFType()),
        (CFString::new(kAudioAggregateDeviceIsStackedKey.to_str().unwrap_or("stacked")),
            CFBoolean::false_value().as_CFType()),
    ];
    let agg_dict = CFDictionary::<CFString, CFType>::from_CFType_pairs(&entries);

    let mut aggr_id: AudioObjectID = 0;
    // Use raw binding to avoid CFDictionary crate mismatch
    let status = unsafe { create_aggregate_device(agg_dict.as_concrete_TypeRef(), &mut aggr_id) };
    if status != 0 {
        unsafe { AudioHardwareDestroyProcessTap(tap_id); }
        return Err(Error::RecordingFailed(format!("AudioHardwareCreateAggregateDevice: {status}")));
    }

    if !wait_for_device(aggr_id) {
        unsafe { AudioHardwareDestroyAggregateDevice(aggr_id); AudioHardwareDestroyProcessTap(tap_id); }
        return Err(Error::RecordingFailed("aggregate device never became alive".into()));
    }

    // ── 5. Create AudioConverter (source format → PCM-16 mono at target rate)
    let src_asbd = tap_fmt;

    let dst_asbd = AudioStreamBasicDescription {
        mSampleRate:       sample_rate as f64,
        mFormatID:         kAudioFormatLinearPCM,
        mFormatFlags:      kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
        mChannelsPerFrame: 1,
        mBitsPerChannel:   16,
        mBytesPerFrame:    2,
        mFramesPerPacket:  1,
        mBytesPerPacket:   2,
        mReserved:         0,
    };

    let needs_conversion = source_rate != sample_rate as f64
        || source_channels != 1
        || (src_asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger == 0)
        || src_asbd.mBitsPerChannel != 16;

    let mut converter: AudioConverterRef = std::ptr::null_mut();
    if needs_conversion {
        let status = unsafe { AudioConverterNew(&src_asbd, &dst_asbd, &mut converter) };
        if status != 0 {
            converter = std::ptr::null_mut(); // non-fatal; proceed without converter
        }
    }

    // ── 6. Box context and register IOProc ────────────────────────────────
    let ctx = Box::new(TapContext {
        tx: tx,
        stopping: Arc::clone(&stopping),
        converter,
        target_rate:      sample_rate as f64,
        source_channels,
        source_bpf,
        conv_input_ptr:   std::ptr::null_mut(),
        conv_input_frames: 0,
        conv_input_bytes:  0,
    });
    let ctx_ptr = Box::into_raw(ctx);
    // Convert to usize so the stop closure can be Send (raw pointers are not Send in Rust 2021).
    let ctx_addr: usize = ctx_ptr as usize;

    // AudioDeviceIOProcID = AudioDeviceIOProc = Option<fn...> — initialise with None
    let mut proc_id: AudioDeviceIOProcID = None;
    let status = unsafe {
        AudioDeviceCreateIOProcID(
            aggr_id,
            Some(audio_io_proc),
            ctx_ptr as *mut c_void,
            NonNull::from(&mut proc_id),
        )
    };
    if status != 0 {
        unsafe {
            drop(Box::from_raw(ctx_ptr));
            if !converter.is_null() { AudioConverterDispose(converter); }
            AudioHardwareDestroyAggregateDevice(aggr_id);
            AudioHardwareDestroyProcessTap(tap_id);
        }
        return Err(Error::RecordingFailed(format!("AudioDeviceCreateIOProcID: {status}")));
    }

    // ── 7. Start device ────────────────────────────────────────────────────
    let status = unsafe { AudioDeviceStart(aggr_id, proc_id) };
    if status != 0 {
        unsafe {
            drop(Box::from_raw(ctx_ptr));
            AudioDeviceDestroyIOProcID(aggr_id, proc_id);
            if !converter.is_null() { AudioConverterDispose(converter); }
            AudioHardwareDestroyAggregateDevice(aggr_id);
            AudioHardwareDestroyProcessTap(tap_id);
        }
        return Err(Error::RecordingFailed(format!(
            "AudioDeviceStart: {status} — check Screen Recording permission in System Settings"
        )));
    }

    // ── 8. Return Recording with teardown closure ──────────────────────────
    let stop_fn = Box::new(move || {
        stopping.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(20)); // let IOProc drain
        unsafe {
            AudioDeviceStop(aggr_id, proc_id);
            AudioDeviceDestroyIOProcID(aggr_id, proc_id);
            let ctx = Box::from_raw(ctx_addr as *mut TapContext);
            if !ctx.converter.is_null() { AudioConverterDispose(ctx.converter); }
            drop(ctx);
            AudioHardwareDestroyAggregateDevice(aggr_id);
            AudioHardwareDestroyProcessTap(tap_id);
        }
    });

    Ok(Recording { rx, stop_fn: Some(stop_fn) })
}
