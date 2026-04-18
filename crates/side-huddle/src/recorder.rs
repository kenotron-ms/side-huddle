    //! Event-emitter based meeting listener.

    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, RwLock};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use crate::mix::mix_recordings;
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

    struct MeetingState {
        in_meeting: bool,
        app:        String,
        recording:  Option<Recording>,
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

        /// Start recording the current meeting.
        ///
        /// Call from within a [`Event::MeetingDetected`] handler to opt in.
        /// No-op if no meeting is active or a recording is already running.
        /// Emits [`Event::RecordingStarted`] on success, [`Event::Error`] on failure.
        pub fn record(&self) {
            let (sample_rate, chunk_ms, output_dir) = {
                let cfg = self.inner.config.lock().unwrap();
                (cfg.sample_rate, cfg.chunk_ms, cfg.output_dir.clone())
            };

            let mut state = self.inner.meeting.lock().unwrap();
            if !state.in_meeting || state.recording.is_some() { return; }

            let app = state.app.clone();

            let tap = match platform::start_tap(sample_rate, chunk_ms) {
                Ok(r)  => r,
                Err(e) => {
                    drop(state);
                    emit(&self.inner, &Event::Error { message: e.to_string() });
                    return;
                }
            };
            let mic = match platform::start_mic(sample_rate, chunk_ms) {
                Ok(r)  => r,
                Err(e) => {
                    drop(state);
                    emit(&self.inner, &Event::Error { message: e.to_string() });
                    return;
                }
            };

            let mixed = mix_recordings(tap, mic, sample_rate);
            let rx    = mixed.rx.clone();
            let path  = output_dir.join(format!("{}-meeting.wav", unix_secs()));
            state.recording = Some(mixed);
            drop(state);

            emit(&self.inner, &Event::RecordingStarted { app: app.clone() });

            let inner = Arc::clone(&self.inner);
            thread::spawn(move || {
                let mut pcm: Vec<i16> = Vec::new();
                for chunk in rx.iter() { pcm.extend_from_slice(&chunk.pcm); }
                if pcm.is_empty() { return; }

                emit(&inner, &Event::RecordingEnded { app: app.clone() });

                if write_wav(&path, &pcm, sample_rate).is_ok() {
                    emit(&inner, &Event::RecordingReady { path, app });
                } else {
                    emit(&inner, &Event::Error {
                        message: format!("failed to write WAV"),
                    });
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
            let sc = check_screen_capture();
            emit(inner, &Event::PermissionStatus {
                permission: Permission::ScreenCapture,
                status:     sc,
            });
            // Microphone: we report NotRequested until the first record() attempt
            emit(inner, &Event::PermissionStatus {
                permission: Permission::Microphone,
                status:     PermissionGranted::NotRequested,
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
        // CGPreflightScreenCaptureAccess() returns true if the process has the
        // Screen Recording permission in System Settings.
        extern "C" { fn CGPreflightScreenCaptureAccess() -> bool; }
        if unsafe { CGPreflightScreenCaptureAccess() } {
            PermissionGranted::Granted
        } else {
            PermissionGranted::NotRequested
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
                emit(inner, &Event::MeetingDetected { app: det.app.clone() });

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
