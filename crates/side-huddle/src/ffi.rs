    //! C ABI — the universal language bridge.
        //!
        //! Produces a `cdylib` (`.dylib` / `.so` / `.dll`) that Go, Python, and any
        //! C-compatible language can link against.
        //!
        //! # Memory contract
        //! * Opaque handles are heap-allocated `MeetingListener` values.  Always free
        //!   them with `side_huddle_free`.
        //! * String pointers inside `SHEvent` are valid **only for the duration of the
        //!   callback**.  Copy them if you need them beyond that.
        //! * `userdata` is never touched by the library — thread-safety is the caller's
        //!   responsibility.

        #![allow(clippy::missing_safety_doc)]

        use std::ffi::{CStr, CString};
        use std::os::raw::{c_char, c_int, c_void};

        use crate::{CaptureKind, Event, MeetingListener, Permission, PermissionGranted};

        // ── Opaque handle helpers ─────────────────────────────────────────────────

        /// Cast a raw handle back to a `&MeetingListener`. Panics on null.
        #[inline]
        unsafe fn as_listener<'a>(h: *const c_void) -> &'a MeetingListener {
            assert!(!h.is_null(), "side_huddle: null handle");
            &*(h as *const MeetingListener)
        }

        // ── C-compatible event kind enum ──────────────────────────────────────────

        #[repr(C)]
        #[derive(Debug, Clone, Copy)]
        pub enum SHEventKind {
            PermissionStatus   = 0,
            PermissionsGranted = 1,
            MeetingDetected    = 2,
            MeetingUpdated     = 3,
            MeetingEnded       = 4,
            RecordingStarted   = 5,
            RecordingEnded     = 6,
            RecordingReady     = 7,
            CaptureStatus      = 8,
            Error              = 9,
        }

        #[repr(C)]
        #[derive(Debug, Clone, Copy)]
        pub enum SHPermission {
            Microphone   = 0,
            ScreenCapture = 1,
            Accessibility = 2,
        }

        #[repr(C)]
        #[derive(Debug, Clone, Copy)]
        pub enum SHPermissionStatus {
            Granted      = 0,
            NotRequested = 1,
            Denied       = 2,
        }

        #[repr(C)]
        #[derive(Debug, Clone, Copy)]
        pub enum SHCaptureKind {
            Audio = 0,
            Video = 1,
        }

        // ── Flat C event struct ───────────────────────────────────────────────────
        //
        // A flat struct is simpler for FFI than a tagged union.
        // Fields not relevant to a given `kind` are NULL / 0.

        #[repr(C)]
        pub struct SHEvent {
            pub kind: SHEventKind,

            // String fields — valid only during the callback
            pub app:     *const c_char,   // meeting app name
            pub title:   *const c_char,   // window title (MeetingUpdated)
            pub path:    *const c_char,   // WAV path (RecordingReady)
            pub message: *const c_char,   // error message (Error)

            // PermissionStatus fields
            pub permission:   SHPermission,
            pub perm_status:  SHPermissionStatus,

            // CaptureStatus fields
            pub capture_kind: SHCaptureKind,
            pub capturing:    c_int,  // 1 = capturing, 0 = interrupted
        }

        /// Callback invoked for every event.  String pointers are valid only for the
        /// duration of this call.  `userdata` is whatever you passed to `side_huddle_on`.
        pub type SHEventCallback =
            unsafe extern "C" fn(event: *const SHEvent, userdata: *mut c_void);

        // ── Rust Event → SHEvent conversion ──────────────────────────────────────

        // Store userdata as usize so the closure is Send + Sync.
        // Cast back to *mut c_void only at the C call boundary.
        struct Userdata(usize);
        unsafe impl Send for Userdata {}
        unsafe impl Sync for Userdata {}

        fn str_ptr(s: &str, buf: &mut Option<CString>) -> *const c_char {
            let cs = CString::new(s).unwrap_or_default();
            let p  = cs.as_ptr();
            *buf   = Some(cs);
            p
        }

        fn dispatch(cb: SHEventCallback, ud: usize, event: &Event) {
            // CStrings live on the stack for the duration of the callback.
            let (mut s1, mut s2, mut s3, s4): (Option<CString>, Option<CString>, Option<CString>, Option<CString>) = Default::default();
            let null: *const c_char = std::ptr::null();

            let ev = match event {
                Event::PermissionStatus { permission, status } => SHEvent {
                    kind:        SHEventKind::PermissionStatus,
                    app: null, title: null, path: null, message: null,
                    permission:  match permission {
                        Permission::Microphone    => SHPermission::Microphone,
                        Permission::ScreenCapture => SHPermission::ScreenCapture,
                        Permission::Accessibility => SHPermission::Accessibility,
                    },
                    perm_status: match status {
                        PermissionGranted::Granted      => SHPermissionStatus::Granted,
                        PermissionGranted::NotRequested => SHPermissionStatus::NotRequested,
                        PermissionGranted::Denied       => SHPermissionStatus::Denied,
                    },
                    capture_kind: SHCaptureKind::Audio,
                    capturing: 0,
                },
                Event::PermissionsGranted => SHEvent {
                    kind: SHEventKind::PermissionsGranted,
                    app: null, title: null, path: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
                Event::MeetingDetected { app } => SHEvent {
                    kind: SHEventKind::MeetingDetected,
                    app: str_ptr(app, &mut s1), title: null, path: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
                Event::MeetingUpdated { app, title } => SHEvent {
                    kind: SHEventKind::MeetingUpdated,
                    app: str_ptr(app, &mut s1), title: str_ptr(title, &mut s2),
                    path: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
                Event::MeetingEnded { app } => SHEvent {
                    kind: SHEventKind::MeetingEnded,
                    app: str_ptr(app, &mut s1), title: null, path: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
                Event::RecordingStarted { app } => SHEvent {
                    kind: SHEventKind::RecordingStarted,
                    app: str_ptr(app, &mut s1), title: null, path: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
                Event::RecordingEnded { app } => SHEvent {
                    kind: SHEventKind::RecordingEnded,
                    app: str_ptr(app, &mut s1), title: null, path: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
                Event::RecordingReady { path, app } => SHEvent {
                    kind: SHEventKind::RecordingReady,
                    app:  str_ptr(app, &mut s1),
                    path: str_ptr(path.to_str().unwrap_or(""), &mut s2),
                    title: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
                Event::CaptureStatus { kind, capturing } => SHEvent {
                    kind: SHEventKind::CaptureStatus,
                    app: null, title: null, path: null, message: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: match kind {
                        CaptureKind::Audio => SHCaptureKind::Audio,
                        CaptureKind::Video => SHCaptureKind::Video,
                    },
                    capturing: if *capturing { 1 } else { 0 },
                },
                Event::Error { message } => SHEvent {
                    kind: SHEventKind::Error,
                    message: str_ptr(message, &mut s3),
                    app: null, title: null, path: null,
                    permission: SHPermission::Microphone, perm_status: SHPermissionStatus::Granted,
                    capture_kind: SHCaptureKind::Audio, capturing: 0,
                },
            };

            // s1..s4 are still alive here and will be dropped at end of scope,
            // which is AFTER the callback returns — pointers in ev remain valid.
            unsafe { cb(&ev, ud as *mut c_void) };
            drop((s1, s2, s3, s4)); // explicit drop for clarity; already the natural order
        }

        // ── Public C API ──────────────────────────────────────────────────────────

        /// Create a new listener.  Free with `side_huddle_free`.
        #[no_mangle]
        pub extern "C" fn side_huddle_new() -> *mut c_void {
            Box::into_raw(Box::new(MeetingListener::new())) as *mut c_void
        }

        /// Free a listener created with `side_huddle_new`.
        ///
        /// # Safety
        /// `handle` must be a valid pointer returned by `side_huddle_new` that has
        /// not previously been freed.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_free(handle: *mut c_void) {
            if !handle.is_null() {
                drop(Box::from_raw(handle as *mut MeetingListener));
            }
        }

        /// Register an event handler.  May be called multiple times — all callbacks
        /// are invoked for every event in registration order.
        ///
        /// `userdata` is passed as-is to every invocation of `callback`.
        /// Thread-safety of `userdata` is the caller's responsibility.
        ///
        /// # Safety
        /// `handle` must be valid.  `callback` must be a valid function pointer.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_on(
            handle:   *const c_void,
            callback: SHEventCallback,
            userdata: *mut c_void,
        ) {
            let ud = Userdata(userdata as usize);
            as_listener(handle).on(move |event| dispatch(callback, ud.0, event));
        }

        /// Automatically record every detected meeting.
        ///
        /// # Safety
        /// `handle` must be valid.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_auto_record(handle: *const c_void) {
            as_listener(handle).auto_record();
        }

        /// Start recording the current meeting (call from within a MeetingDetected callback).
        ///
        /// # Safety
        /// `handle` must be valid.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_record(handle: *const c_void) {
            as_listener(handle).record();
        }

        /// Set the PCM sample rate (default: 16 000 Hz).  Call before `side_huddle_start`.
        ///
        /// # Safety
        /// `handle` must be valid.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_set_sample_rate(
            handle: *const c_void,
            hz:     u32,
        ) {
            as_listener(handle).sample_rate(hz);
        }

        /// Set the WAV output directory (default: current working directory).
        /// `dir` must be a valid UTF-8 null-terminated string.  Call before `side_huddle_start`.
        ///
        /// # Safety
        /// `handle` and `dir` must be valid.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_set_output_dir(
            handle: *const c_void,
            dir:    *const c_char,
        ) {
            if dir.is_null() { return; }
            if let Ok(s) = CStr::from_ptr(dir).to_str() {
                as_listener(handle).output_dir(s);
            }
        }

        /// Start monitoring.  Returns 0 on success, non-zero on failure.
        ///
        /// # Safety
        /// `handle` must be valid.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_start(handle: *const c_void) -> c_int {
            match as_listener(handle).start() {
                Ok(())  => 0,
                Err(_)  => -1,
            }
        }

        /// Stop monitoring and any active recording.
        ///
        /// # Safety
        /// `handle` must be valid.
        #[no_mangle]
        pub unsafe extern "C" fn side_huddle_stop(handle: *const c_void) {
            as_listener(handle).stop();
        }

        /// Return the library version string (static, never free).
        #[no_mangle]
        pub extern "C" fn side_huddle_version() -> *const c_char {
            static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
            VERSION.as_ptr() as *const c_char
        }
    