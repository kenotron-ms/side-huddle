    //! Event-emitter based meeting listener.

    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, RwLock};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use crate::mix::mix_pcm;
    use crate::monitor::Monitor;
    use crate::platform;
    use crate::{Detection, DetectionKind, Event, Permission, PermissionGranted, Recording, Result};

    // ── Public type ───────────────────────────────────────────────────────────

    /// Detects meetings and emits lifecycle events.
    ///
    /// Cheaply cloneable — all clones share the same state.  Capture a clone
    /// inside an `on` handler to call [`record`](Self::record) or [`stop`](Self::stop).
    #[derive(Clone)]
    pub struct MeetingListener {
        inner: Arc<Inner>,
    }

    struct Inner {
        config:      Mutex<Config>,
        handlers:    RwLock<Vec<Box<dyn Fn(&Event) + Send + Sync + 'static>>>,
        auto_record: AtomicBool,
        meeting:     Mutex<MeetingState>,
        monitor:     Mutex<Option<Monitor>>,
    }

    struct Config {
        sample_rate: u32,
        chunk_ms:    u32,
        output_dir:  PathBuf,
    }

    impl Default for Config {
        fn default() -> Self {
            Self {
                sample_rate: 16_000,
                chunk_ms:    200,
                output_dir:  std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            }
        }
    }

    /// Holds the stop functions for an active recording.
    /// Dropping this calls both tap and mic stop functions.
    struct RecordingHandle {
        _tap_stop: Option<Box<dyn FnOnce() + Send>>,
        _mic_stop: Option<Box<dyn FnOnce() + Send>>,
    }

    impl Drop for RecordingHandle {
        fn drop(&mut self) {
            if let Some(f) = self._tap_stop.take() { f(); }
            if let Some(f) = self._mic_stop.take() { f(); }
        }
    }

    struct MeetingState {
        in_meeting: bool,
        app:        String,
        recording:  Option<RecordingHandle>,
    }

    // ── Public API ────────────────────────────────────────────────────────────

    impl MeetingListener {
        /// Create a listener with default settings (16 kHz, current directory).
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Inner {
                    config:      Mutex::new(Config::default()),
                    handlers:    RwLock::new(Vec::new()),
                    auto_record: AtomicBool::new(false),
                    meeting:     Mutex::new(MeetingState {
                        in_meeting: false,
                        app:        String::new(),
                        recording:  None,
                    }),
                    monitor: Mutex::new(None),
                }),
            }
        }

        /// Set PCM sample rate (default: 16 000 Hz). Call before [`start`](Self::start).
        pub fn sample_rate(&self, hz: u32) -> &Self {
            self.inner.config.lock().unwrap().sample_rate = hz;
            self
        }

        /// Set WAV output directory (default: cwd). Call before [`start`](Self::start).
        pub fn output_dir<P: Into<PathBuf>>(&self, dir: P) -> &Self {
            self.inner.config.lock().unwrap().output_dir = dir.into();
            self
        }

        /// Register an event handler.  All registered handlers receive every
        /// event in registration order — register as many as you need.
        ///
        /// Clone `self` to call [`record`](Self::record) from inside a handler:
        /// ```no_run
        /// # use side_huddle::{MeetingListener, Event};
        /// let listener = MeetingListener::new();
        /// let l = listener.clone();
        /// listener.on(move |e| {
        ///     if let Event::MeetingDetected { .. } = e { l.record(); }
        /// });
        /// ```
        pub fn on<F: Fn(&Event) + Send + Sync + 'static>(&self, f: F) -> &Self {
            self.inner.handlers.write().unwrap().push(Box::new(f));
            self
        }

        /// Record every detected meeting automatically — no need to call
        /// [`record`](Self::record) from a handler.
        pub fn auto_record(&self) -> &Self {
            self.inner.auto_record.store(true, Ordering::Relaxed);
            self
        }

        /// Open System Settings to grant the permissions required for recording.
        ///
        /// On macOS, Screen Recording cannot be requested via an inline dialog — the OS
        /// only provides a way to open System Settings → Privacy & Security → Screen
        /// Recording.  After the user grants access there and the app restarts (or calls
        /// [`start`](Self::start) again), recording will succeed.
        ///
        /// Microphone permission is handled separately: the inline OS dialog is shown
        /// automatically the first time [`record`](Self::record) is called.
        ///
        /// Emits [`Event::PermissionStatus`] for each permission and
        /// [`Event::PermissionsGranted`] if Screen Capture is already granted.
        /// Safe to call multiple times (idempotent).
        pub fn request_permissions(&self) {
            #[cfg(target_os = "macos")]
            {
                // CGRequestScreenCaptureAccess: returns true if already granted,
                // otherwise opens System Settings → Screen Recording and returns false.
                // There is no blocking inline dialog for this permission — the user must
                // grant it manually, then the listener needs to be restarted.
                extern "C" { fn CGRequestScreenCaptureAccess() -> bool; }
                unsafe { CGRequestScreenCaptureAccess(); }
            }
            // Re-check all permissions and broadcast current status.
            // Emits PermissionsGranted if Screen Capture is now (or already was) granted.
            check_and_emit_permissions(&self.inner);
        }

        /// Start recording the current meeting.
        ///
        /// Call from within a [`Event::MeetingDetected`] handler to opt in.
        /// No-op if no meeting is active or a recording is already running.
        /// Emits [`Event::RecordingStarted`] on success, [`Event::Error`] on failure.
        pub fn record(&self) {
            // macOS: check Screen Recording first — it's the hard gate for the audio
            // tap. If it's denied, bail immediately rather than showing a microphone
            // dialog that would only confuse the user (the tap fails regardless).
            #[cfg(target_os = "macos")]
            {
                if check_screen_capture() == PermissionGranted::Denied {
                    emit(&self.inner, &Event::PermissionStatus {
                        permission: Permission::ScreenCapture,
                        status:     PermissionGranted::Denied,
                    });
                    emit(&self.inner, &Event::Error {
                        message: "Screen Recording access required — call request_permissions() \
                                  to open System Settings, grant access, then restart the listener"
                            .into(),
                    });
                    return;
                }
            }

            // macOS: check/request microphone permission outside all locks so we can
            // safely block for the dialog.
            #[cfg(target_os = "macos")]
            {
                match check_microphone() {
                    PermissionGranted::Granted => {}
                    PermissionGranted::NotRequested => {
                        // Never been asked — show the system dialog now.
                        let status = request_microphone_access();
                        emit(&self.inner, &Event::PermissionStatus {
                            permission: Permission::Microphone,
                            status,
                        });
                        if status != PermissionGranted::Granted {
                            emit(&self.inner, &Event::Error {
                                message: "Microphone access denied — grant permission in System Settings > Privacy > Microphone".into(),
                            });
                            return;
                        }
                    }
                    PermissionGranted::Denied => {
                        emit(&self.inner, &Event::PermissionStatus {
                            permission: Permission::Microphone,
                            status: PermissionGranted::Denied,
                        });
                        emit(&self.inner, &Event::Error {
                            message: "Microphone access denied — grant permission in System Settings > Privacy > Microphone".into(),
                        });
                        return;
                    }
                }
            }

            let (sample_rate, chunk_ms, output_dir) = {
                let cfg = self.inner.config.lock().unwrap();
                (cfg.sample_rate, cfg.chunk_ms, cfg.output_dir.clone())
            };

            let mut state = self.inner.meeting.lock().unwrap();
            if !state.in_meeting || state.recording.is_some() { return; }

            let app = state.app.clone();

            let tap = match platform::start_tap(sample_rate, chunk_ms) {
                Ok(r)  => r,
                Err(e) => { drop(state); emit(&self.inner, &Event::Error { message: e.to_string() }); return; }
            };
            let mic = match platform::start_mic(sample_rate, chunk_ms) {
                Ok(r)  => r,
                Err(e) => { drop(state); emit(&self.inner, &Event::Error { message: e.to_string() }); return; }
            };

            // Extract stop functions and receivers before consuming the Recording objects
            let mut tap = tap; let mut mic = mic;
            let tap_stop = tap.stop_fn.take();
            let mic_stop = mic.stop_fn.take();
            let tap_rx   = tap.rx.clone();
            let mic_rx   = mic.rx.clone();
            drop(tap); drop(mic);

            let stem         = output_dir.join(format!("{}-meeting", unix_secs()));
            let mixed_path   = stem.with_extension("wav");
            let others_path  = PathBuf::from(format!("{}-others.wav",  stem.display()));
            let self_path    = PathBuf::from(format!("{}-self.wav",    stem.display()));

            // Store stop handles so stop_recording() can halt both streams
            state.recording = Some(RecordingHandle {
                _tap_stop: tap_stop,
                _mic_stop: mic_stop,
            });
            drop(state);

            emit(&self.inner, &Event::RecordingStarted { app: app.clone() });

            let inner = Arc::clone(&self.inner);
            thread::spawn(move || {
                // Drain tap and mic concurrently into separate PCM buffers
                use std::sync::mpsc::sync_channel;
                let (tap_tx, tap_done) = sync_channel::<Vec<i16>>(0);
                let (mic_tx, mic_done) = sync_channel::<Vec<i16>>(0);

                thread::spawn(move || {
                    let mut pcm = Vec::new();
                    for chunk in tap_rx { pcm.extend_from_slice(&chunk.pcm); }
                    let _ = tap_tx.send(pcm);
                });
                thread::spawn(move || {
                    let mut pcm = Vec::new();
                    for chunk in mic_rx { pcm.extend_from_slice(&chunk.pcm); }
                    let _ = mic_tx.send(pcm);
                });

                let others_pcm = tap_done.recv().unwrap_or_default();
                let self_pcm   = mic_done.recv().unwrap_or_default();

                if others_pcm.is_empty() && self_pcm.is_empty() { return; }

                let mixed_pcm = mix_pcm(&others_pcm, &self_pcm);

                emit(&inner, &Event::RecordingEnded { app: app.clone() });

                let ok = write_wav(&others_path, &others_pcm, sample_rate).is_ok()
                    &    write_wav(&self_path,   &self_pcm,   sample_rate).is_ok()
                    &    write_wav(&mixed_path,  &mixed_pcm,  sample_rate).is_ok();

                if ok {
                    emit(&inner, &Event::RecordingReady {
                        mixed_path,
                        others_path,
                        self_path,
                        app,
                    });
                } else {
                    emit(&inner, &Event::Error { message: "failed to write WAV files".into() });
                }
            });
        }

        /// Start monitoring.  Emits [`Event::PermissionStatus`] ×N and
        /// [`Event::PermissionsGranted`] before the first detection event.
        pub fn start(&self) -> Result<()> {
            // Check and emit permission status (macOS only; instant on other platforms)
            check_and_emit_permissions(&self.inner);

            let mut mon    = Monitor::new();
            let inner_ref  = Arc::clone(&self.inner);

            mon.on_detection(move |det: Detection| {
                on_detection(&inner_ref, det);
            });

            mon.start()?;
            *self.inner.monitor.lock().unwrap() = Some(mon);
            Ok(())
        }

        /// Stop monitoring and cancel any active recording.
        pub fn stop(&self) {
            if let Some(mon) = self.inner.monitor.lock().unwrap().take() {
                mon.stop();
            }
            self.inner.meeting.lock().unwrap().recording = None;
        }
    }

    impl Default for MeetingListener {
        fn default() -> Self { Self::new() }
    }

    // ── Permission checking ───────────────────────────────────────────────────

    fn check_and_emit_permissions(inner: &Arc<Inner>) {
        #[cfg(target_os = "macos")]
        {
            let sc  = check_screen_capture();
            let mic = check_microphone();
            emit(inner, &Event::PermissionStatus {
                permission: Permission::ScreenCapture,
                status:     sc,
            });
            emit(inner, &Event::PermissionStatus {
                permission: Permission::Microphone,
                status:     mic,
            });
            if sc == PermissionGranted::Granted {
                emit(inner, &Event::PermissionsGranted);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Windows / Linux need no system permissions for audio capture
            emit(inner, &Event::PermissionsGranted);
        }
    }

    #[cfg(target_os = "macos")]
    fn check_screen_capture() -> PermissionGranted {
        extern "C" { fn CGPreflightScreenCaptureAccess() -> bool; }
        if unsafe { CGPreflightScreenCaptureAccess() } {
            PermissionGranted::Granted
        } else {
            // CGPreflightScreenCaptureAccess() returns false for BOTH "never asked"
            // and "explicitly denied" — no public API distinguishes them.
            // Map both to Denied: the app cannot tap system audio in either case,
            // and downstream code should show "Permission needed" rather than
            // offering "Record & Transcribe" (which would fail at start_tap() anyway
            // and incorrectly trigger the mic dialog along the way).
            PermissionGranted::Denied
        }
    }

    /// Check microphone permission via the ObjC runtime without requiring
    /// AVFoundation to be an explicit Rust dependency.
    /// Calls [AVCaptureDevice authorizationStatusForMediaType: @"soun"].
    /// Returns: NotRequested=not yet asked, Denied=blocked, Granted=approved.
    #[cfg(target_os = "macos")]
    fn check_microphone() -> PermissionGranted {
        use std::ffi::c_void;
        type ID  = *mut c_void;
        type SEL = *const c_void;

        extern "C" {
            fn objc_getClass(name: *const u8)    -> *const c_void;
            fn sel_registerName(name: *const u8) -> SEL;
        }

        let msg_send_ptr = unsafe {
            libc::dlsym(libc::RTLD_DEFAULT, b"objc_msgSend\0".as_ptr() as _)
        };
        if msg_send_ptr.is_null() { return PermissionGranted::NotRequested; }

        // Ensure AVFoundation is loaded — on macOS 14+ classes register lazily
        // even when the binary links the framework.
        unsafe {
            libc::dlopen(
                b"/System/Library/Frameworks/AVFoundation.framework/AVFoundation\0".as_ptr() as *const libc::c_char,
                libc::RTLD_LAZY | libc::RTLD_GLOBAL,
            );
        }

        unsafe {
            let ns_string_cls = objc_getClass(b"NSString\0".as_ptr());
            let av_device_cls = objc_getClass(b"AVCaptureDevice\0".as_ptr());
            if ns_string_cls.is_null() || av_device_cls.is_null() {
                return PermissionGranted::NotRequested;
            }

            // [NSString stringWithUTF8String:"soun"]  (AVMediaTypeAudio constant)
            let sel_utf8 = sel_registerName(b"stringWithUTF8String:\0".as_ptr());
            type FnStr = unsafe extern "C" fn(*const c_void, SEL, *const u8) -> ID;
            let fn_str: FnStr = std::mem::transmute(msg_send_ptr);
            let media_type = fn_str(ns_string_cls, sel_utf8, b"soun\0".as_ptr());
            if media_type.is_null() { return PermissionGranted::NotRequested; }

            // [AVCaptureDevice authorizationStatusForMediaType: mediaType]
            // NSInteger: 0=NotDetermined, 1=Restricted, 2=Denied, 3=Authorized
            let sel_auth = sel_registerName(b"authorizationStatusForMediaType:\0".as_ptr());
            type FnAuth = unsafe extern "C" fn(*const c_void, SEL, ID) -> isize;
            let fn_auth: FnAuth = std::mem::transmute(msg_send_ptr);
            match fn_auth(av_device_cls, sel_auth, media_type) {
                3 => PermissionGranted::Granted,
                1 | 2 => PermissionGranted::Denied,
                _ => PermissionGranted::NotRequested,  // 0 = not yet determined
            }
        }
    }

    /// Synchronously request microphone access via
    /// [AVCaptureDevice requestAccessForMediaType:completionHandler:].
    /// Blocks the calling thread until the user responds to the system dialog.
    /// Safe to call even if permission was already granted (returns immediately).
    #[cfg(target_os = "macos")]
    fn request_microphone_access() -> PermissionGranted {
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicBool, Ordering};

        type ID  = *mut c_void;
        type SEL = *const c_void;

        extern "C" {
            fn objc_getClass(name: *const u8)              -> *const c_void;
            fn sel_registerName(name: *const u8)           -> SEL;
            fn dispatch_semaphore_create(value: isize)     -> *mut c_void;
            fn dispatch_semaphore_signal(sema: *mut c_void) -> isize;
            fn dispatch_semaphore_wait(sema: *mut c_void, timeout: u64) -> isize;
            fn dispatch_release(obj: *mut c_void);
        }

        // Objective-C block ABI: void(^)(BOOL)
        // Flags = 0 → no copy/dispose helpers → ObjC does a bitwise copy of the struct,
        // which is exactly what we want (the pointers remain valid throughout the wait).
        #[repr(C)]
        struct BoolBlock {
            isa:      *const c_void,
            flags:    i32,
            reserved: i32,
            invoke:   unsafe extern "C" fn(*const BoolBlock, bool),
            desc:     *const BlockDesc,
            granted:  *const AtomicBool,   // captured: where to write the result
            sema:     *mut c_void,         // captured: semaphore to signal
        }

        #[repr(C)]
        struct BlockDesc { reserved: usize, size: usize }
        static BLOCK_DESC: BlockDesc = BlockDesc {
            reserved: 0,
            size:     core::mem::size_of::<BoolBlock>(),
        };

        unsafe extern "C" fn block_invoke(block: *const BoolBlock, granted: bool) {
            (*(*block).granted).store(granted, Ordering::SeqCst);
            dispatch_semaphore_signal((*block).sema);
        }

        let msg_send_ptr = unsafe {
            libc::dlsym(libc::RTLD_DEFAULT, b"objc_msgSend\0".as_ptr() as _)
        };
        let stack_block_isa = unsafe {
            libc::dlsym(libc::RTLD_DEFAULT, b"_NSConcreteStackBlock\0".as_ptr() as _)
        };
        if msg_send_ptr.is_null() || stack_block_isa.is_null() {
            return PermissionGranted::NotRequested;
        }

        // Force AVFoundation class registration before calling objc_getClass.
        unsafe {
            libc::dlopen(
                b"/System/Library/Frameworks/AVFoundation.framework/AVFoundation\0".as_ptr() as *const libc::c_char,
                libc::RTLD_LAZY | libc::RTLD_GLOBAL,
            );
        }

        let granted = AtomicBool::new(false);

        unsafe {
            let sema = dispatch_semaphore_create(0);
            if sema.is_null() { return PermissionGranted::NotRequested; }

            let mut block = BoolBlock {
                isa:      stack_block_isa,
                flags:    0,
                reserved: 0,
                invoke:   block_invoke,
                desc:     &BLOCK_DESC,
                granted:  &granted,
                sema,
            };

            let ns_string_cls = objc_getClass(b"NSString\0".as_ptr());
            let av_device_cls = objc_getClass(b"AVCaptureDevice\0".as_ptr());
            if ns_string_cls.is_null() || av_device_cls.is_null() {
                dispatch_release(sema);
                return PermissionGranted::NotRequested;
            }

            let sel_utf8 = sel_registerName(b"stringWithUTF8String:\0".as_ptr());
            type FnStr = unsafe extern "C" fn(*const c_void, SEL, *const u8) -> ID;
            let fn_str: FnStr = core::mem::transmute(msg_send_ptr);
            let media_type = fn_str(ns_string_cls, sel_utf8, b"soun\0".as_ptr());

            let sel_req = sel_registerName(b"requestAccessForMediaType:completionHandler:\0".as_ptr());
            type FnReq = unsafe extern "C" fn(*const c_void, SEL, ID, *mut BoolBlock);
            let fn_req: FnReq = core::mem::transmute(msg_send_ptr);
            fn_req(av_device_cls, sel_req, media_type, &mut block);

            // Wait for the user to respond (or already-granted to call the handler immediately)
            dispatch_semaphore_wait(sema, u64::MAX);
            dispatch_release(sema);
        }

        if granted.load(Ordering::SeqCst) {
            PermissionGranted::Granted
        } else {
            PermissionGranted::Denied
        }
    }


    // ── Detection dispatch ────────────────────────────────────────────────────

    fn on_detection(inner: &Arc<Inner>, det: Detection) {
        match det.kind {
            DetectionKind::Started => {
                {
                    let mut m = inner.meeting.lock().unwrap();
                    m.in_meeting = true;
                    m.app        = det.app.clone();
                }
                emit(inner, &Event::MeetingDetected { app: det.app.clone(), pid: det.pid });

                if inner.auto_record.load(Ordering::Relaxed) {
                    MeetingListener { inner: Arc::clone(inner) }.record();
                }
            }

            DetectionKind::Updated => {
                // Window title became known — emit MeetingUpdated
                if let Some(title) = det.title {
                    emit(inner, &Event::MeetingUpdated { app: det.app, title });
                }
            }

            DetectionKind::Ended => {
                // Stop any running recording (closes tap → accumulation thread
                // notices channel disconnect → emits RecordingEnded + RecordingReady)
                inner.meeting.lock().unwrap().recording = None;
                emit(inner, &Event::MeetingEnded { app: det.app });
                inner.meeting.lock().unwrap().in_meeting = false;
            }

            DetectionKind::SpeakerChanged => {
                emit(inner, &Event::SpeakerChanged { speakers: det.speakers, app: det.app });
            }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn emit(inner: &Arc<Inner>, event: &Event) {
        let handlers = inner.handlers.read().unwrap();
        for h in handlers.iter() { h(event); }
    }

    fn unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn write_wav(path: &Path, pcm: &[i16], sample_rate: u32) -> std::io::Result<()> {
        let mut f     = std::fs::File::create(path)?;
        let data_len  = (pcm.len() * 2) as u32;
        let byte_rate = sample_rate * 2;

        f.write_all(b"RIFF")?;
        f.write_all(&(36 + data_len).to_le_bytes())?;
        f.write_all(b"WAVE")?;
        f.write_all(b"fmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?;
        f.write_all(&sample_rate.to_le_bytes())?;
        f.write_all(&byte_rate.to_le_bytes())?;
        f.write_all(&2u16.to_le_bytes())?;
        f.write_all(&16u16.to_le_bytes())?;
        f.write_all(b"data")?;
        f.write_all(&data_len.to_le_bytes())?;
        for &s in pcm { f.write_all(&s.to_le_bytes())?; }
        Ok(())
    }
