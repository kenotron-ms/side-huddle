/// Microphone capture via AVAudioEngine — works on macOS 14+ (Sonoma) and macOS 26 (Tahoe).
///
/// cpal 0.17 uses AudioUnit kAudioUnitSubType_RemoteIO for input, which was
/// silently broken in macOS 26 Tahoe.  AVAudioEngine is Apple's modern replacement
/// and correctly honours microphone permissions on all recent macOS versions.
use std::ffi::c_void;
use crossbeam_channel::bounded;

use crate::{AudioChunk, Error, Recording, Result};

// ── ObjC runtime helpers ─────────────────────────────────────────────────────

type ID  = *mut c_void;
type SEL = *const c_void;

extern "C" {
    fn objc_getClass(name: *const u8) -> *const c_void;
    fn sel_registerName(name: *const u8) -> SEL;
}

fn msgsend() -> usize {
    unsafe { libc::dlsym(libc::RTLD_DEFAULT, b"objc_msgSend\0".as_ptr() as _) as usize }
}

unsafe fn call0(ptr: usize, recv: *const c_void, sel: SEL) -> ID {
    let f: unsafe extern "C" fn(*const c_void, SEL) -> ID = std::mem::transmute(ptr);
    f(recv, sel)
}
unsafe fn call0f(ptr: usize, recv: *const c_void, sel: SEL) -> f64 {
    let f: unsafe extern "C" fn(*const c_void, SEL) -> f64 = std::mem::transmute(ptr);
    f(recv, sel)
}
unsafe fn call0u(ptr: usize, recv: *const c_void, sel: SEL) -> u32 {
    let f: unsafe extern "C" fn(*const c_void, SEL) -> u32 = std::mem::transmute(ptr);
    f(recv, sel)
}
unsafe fn call_engine_start(ptr: usize, recv: *const c_void, sel: SEL, err_ptr: *mut ID) -> bool {
    let f: unsafe extern "C" fn(*const c_void, SEL, *mut ID) -> bool = std::mem::transmute(ptr);
    f(recv, sel, err_ptr)
}
unsafe fn call_stop(ptr: usize, recv: *const c_void, sel: SEL) {
    let f: unsafe extern "C" fn(*const c_void, SEL) = std::mem::transmute(ptr);
    f(recv, sel)
}

// ── Tap block ABI: void(^)(AVAudioPCMBuffer*, AVAudioTime*) ──────────────────

#[repr(C)]
struct TapBlock {
    isa:        *const c_void,
    flags:      i32,
    reserved:   i32,
    invoke:     unsafe extern "C" fn(*const TapBlock, ID, ID),
    descriptor: *const BlockDesc,
    state:      *mut TapState,
}

#[repr(C)]
struct BlockDesc { reserved: usize, size: usize }
static TAP_BLOCK_DESC: BlockDesc = BlockDesc {
    reserved: 0,
    size: core::mem::size_of::<TapBlock>(),
};

struct TapState {
    tx:                   crossbeam_channel::Sender<AudioChunk>,
    buf:                  Vec<i16>,
    native_rate:          u32,
    native_ch:            u32,
    target_rate:          u32,
    target_frames:        usize,
    native_frames_chunk:  usize,
    msg_send_ptr:         usize,
    sel_float_data:       SEL,
    sel_frame_len:        SEL,
}

unsafe extern "C" fn tap_invoke(block: *const TapBlock, buffer: ID, _when: ID) {
    let state = &mut *(*block).state;
    let ms    = state.msg_send_ptr;

    let float_data: *const *const f32 = {
        let f: unsafe extern "C" fn(ID, SEL) -> *const *const f32 =
            std::mem::transmute(ms);
        f(buffer, state.sel_float_data)
    };
    let frame_len: u32 = call0u(ms, buffer, state.sel_frame_len);

    if float_data.is_null() || frame_len == 0 { return; }

    let ch = state.native_ch as usize;
    for i in 0..frame_len as usize {
        let mut sum = 0f32;
        for c in 0..ch {
            let ch_ptr = *float_data.add(c);
            if !ch_ptr.is_null() { sum += *ch_ptr.add(i); }
        }
        let mono = (sum / ch as f32).clamp(-1.0, 1.0);
        state.buf.push((mono * i16::MAX as f32) as i16);
    }

    while state.buf.len() >= state.native_frames_chunk {
        let native: Vec<i16> = state.buf.drain(..state.native_frames_chunk).collect();
        let pcm: Vec<i16> = if state.native_rate != state.target_rate {
            let ratio = state.native_rate as f64 / state.target_rate as f64;
            (0..state.target_frames)
                .map(|i| {
                    let src = ((i as f64 * ratio) as usize).min(native.len() - 1);
                    native[src]
                })
                .collect()
        } else {
            native
        };
        let _ = state.tx.try_send(AudioChunk { pcm });
    }
}

// ── Send-safe raw pointer wrapper ─────────────────────────────────────────────

struct RawPtr<T>(*mut T);
unsafe impl<T> Send for RawPtr<T> {}
impl<T> RawPtr<T> { fn get(&self) -> *mut T { self.0 } }

// ── Public entry point ────────────────────────────────────────────────────────

pub(crate) fn start(sample_rate: u32, chunk_ms: u32) -> Result<Recording> {
    // Ensure AVFoundation is loaded — on macOS 14+ classes register lazily.
    unsafe {
        libc::dlopen(
            b"/System/Library/Frameworks/AVFoundation.framework/AVFoundation\0".as_ptr()
                as *const libc::c_char,
            libc::RTLD_LAZY | libc::RTLD_GLOBAL,
        );
    }

    let ms = msgsend();
    if ms == 0 {
        return Err(Error::RecordingFailed("objc_msgSend not found".into()));
    }

    let engine = unsafe {
        let cls = objc_getClass(b"AVAudioEngine\0".as_ptr());
        if cls.is_null() {
            return Err(Error::RecordingFailed("AVAudioEngine class not found — AVFoundation not loaded".into()));
        }
        let obj = call0(ms, cls, sel_registerName(b"alloc\0".as_ptr()));
        call0(ms, obj, sel_registerName(b"init\0".as_ptr()))
    };
    if engine.is_null() {
        return Err(Error::RecordingFailed("AVAudioEngine alloc/init failed".into()));
    }

    let (input_node, native_rate, native_ch) = unsafe {
        let node = call0(ms, engine, sel_registerName(b"inputNode\0".as_ptr()));
        if node.is_null() {
            return Err(Error::RecordingFailed("AVAudioEngine.inputNode is nil".into()));
        }
        let fmt: ID = {
            let f: unsafe extern "C" fn(*const c_void, SEL, u64) -> ID =
                std::mem::transmute(ms);
            f(node, sel_registerName(b"inputFormatForBus:\0".as_ptr()), 0)
        };
        if fmt.is_null() {
            return Err(Error::RecordingFailed("could not get input format from AVAudioEngine".into()));
        }
        let sr: f64 = call0f(ms, fmt, sel_registerName(b"sampleRate\0".as_ptr()));
        let ch: u32 = call0u(ms, fmt, sel_registerName(b"channelCount\0".as_ptr()));
        (node, sr as u32, ch.max(1))
    };

    let target_frames           = (sample_rate * chunk_ms / 1000) as usize;
    let native_frames_per_chunk = if native_rate != sample_rate {
        (target_frames as f64 * native_rate as f64 / sample_rate as f64).ceil() as usize
    } else {
        target_frames
    };

    let (tx, rx) = bounded::<AudioChunk>(64);

    let state = Box::into_raw(Box::new(TapState {
        tx:                  tx.clone(),
        buf:                 Vec::with_capacity(native_frames_per_chunk * 2),
        native_rate,
        native_ch,
        target_rate:         sample_rate,
        target_frames,
        native_frames_chunk: native_frames_per_chunk,
        msg_send_ptr:        ms,
        sel_float_data:      unsafe { sel_registerName(b"floatChannelData\0".as_ptr()) },
        sel_frame_len:       unsafe { sel_registerName(b"frameLength\0".as_ptr()) },
    }));

    let stack_block_isa = unsafe {
        libc::dlsym(libc::RTLD_DEFAULT, b"_NSConcreteStackBlock\0".as_ptr() as _)
    };
    if stack_block_isa.is_null() {
        unsafe { drop(Box::from_raw(state)); }
        return Err(Error::RecordingFailed("_NSConcreteStackBlock not found".into()));
    }

    let mut tap_block = TapBlock {
        isa:        stack_block_isa,
        flags:      0,
        reserved:   0,
        invoke:     tap_invoke,
        descriptor: &TAP_BLOCK_DESC,
        state,
    };

    unsafe {
        let sel_tap = sel_registerName(b"installTapOnBus:bufferSize:format:block:\0".as_ptr());
        type FnTap = unsafe extern "C" fn(*const c_void, SEL, u64, u32, ID, *mut TapBlock);
        let fn_tap: FnTap = std::mem::transmute(ms);
        fn_tap(input_node, sel_tap, 0, 4096, std::ptr::null_mut(), &mut tap_block);
    }

    unsafe {
        let mut error: ID = std::ptr::null_mut();
        let ok = call_engine_start(ms, engine, sel_registerName(b"startAndReturnError:\0".as_ptr()), &mut error);
        if !ok {
            let desc = error_description(ms, error);
            drop(Box::from_raw(state));
            return Err(Error::RecordingFailed(format!("AVAudioEngine start failed: {desc}")));
        }
    }

    let engine_ptr     = RawPtr(engine as *mut c_void);
    let input_node_ptr = RawPtr(input_node as *mut c_void);
    let state_ptr      = RawPtr(state);

    let stop_fn: Box<dyn FnOnce() + Send> = Box::new(move || {
        unsafe {
            let engine     = engine_ptr.get() as *const c_void;
            let input_node = input_node_ptr.get() as *const c_void;
            let state      = state_ptr.get();

            let f: unsafe extern "C" fn(*const c_void, SEL, u64) = std::mem::transmute(ms);
            f(input_node, sel_registerName(b"removeTapOnBus:\0".as_ptr()), 0);

            call_stop(ms, engine, sel_registerName(b"stop\0".as_ptr()));
            drop(Box::from_raw(state));
        }
        drop(tx);
    });

    Ok(Recording { rx, stop_fn: Some(stop_fn) })
}

unsafe fn error_description(ms: usize, error: ID) -> String {
    if error.is_null() { return "unknown error".into(); }
    let sel_desc = sel_registerName(b"localizedDescription\0".as_ptr());
    let desc = call0(ms, error, sel_desc);
    if desc.is_null() { return "?".into(); }
    let f: unsafe extern "C" fn(*const c_void, SEL) -> *const u8 = std::mem::transmute(ms);
    let ptr = f(desc, sel_registerName(b"UTF8String\0".as_ptr()));
    if ptr.is_null() { return "?".into(); }
    std::ffi::CStr::from_ptr(ptr as *const i8)
        .to_str().unwrap_or("?").to_owned()
}
