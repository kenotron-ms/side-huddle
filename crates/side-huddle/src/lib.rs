    //! Detect a Teams / Zoom / Google Meet session and deliver a WAV recording.
    //!
    //! # Quick start
    //! ```no_run
    //! use side_huddle::{MeetingListener, Event};
    //!
    //! let listener = MeetingListener::new();
    //!
    //! listener.on(|event| println!("{event:?}"));
    //!
    //! let l = listener.clone();
    //! listener.on(move |event| {
    //!     if let Event::MeetingDetected { .. } = event { l.record(); }
    //! });
    //!
    //! listener.start().unwrap();
    //! std::thread::park();
    //! ```

    mod apps;
    mod ffi;
    mod mix;
    mod monitor;
    mod platform;
    mod recorder;

    pub use recorder::MeetingListener;

    /// Window utilities re-exported for use from examples and external consumers.
    #[cfg(target_os = "macos")]
    pub mod window {
        pub use crate::platform::darwin::window::{
            cg_window_owner, find_primary_window, window_bounds, window_exists,
        };
    }

    // ── Public event type ─────────────────────────────────────────────────────

    /// All events emitted by [`MeetingListener`].
    ///
    /// Register handlers with [`MeetingListener::on`].
    /// Multiple handlers for the same event are all called in registration order.
    ///
    /// Lifecycle order for a recorded meeting:
    /// ```text
    /// PermissionStatus × N  (macOS only, on start)
    /// PermissionsGranted    (macOS only, once all perms OK)
    /// MeetingDetected       (meeting begins)
    /// MeetingUpdated        (title becomes known via window scan)
    /// RecordingStarted      (if record() was called)
    /// MeetingEnded          (meeting stops)
    /// RecordingEnded        (capture stopped, WAV being written)
    /// RecordingReady        (WAV file written to disk)
    /// ```
    #[derive(Debug, Clone)]
    pub enum Event {
        // ── Permissions ───────────────────────────────────────────────────────
        /// Status of an individual permission, emitted once per permission on
        /// [`MeetingListener::start`].  macOS only; not emitted on Windows / Linux
        /// where no permissions are required.
        PermissionStatus {
            permission: Permission,
            status:     PermissionGranted,
        },

        /// All required permissions are granted; recording can proceed.
        /// Emitted immediately on non-macOS platforms.
        PermissionsGranted,

        // ── Meeting lifecycle ─────────────────────────────────────────────────
        /// A Teams / Zoom / Google Meet session was detected (new start, or
        /// already in progress when the listener started).
        MeetingDetected { app: String },

        /// Meeting metadata became known — currently the window title once the
        /// window watcher identifies the call window.
        MeetingUpdated { app: String, title: String },

        /// The meeting has ended.
        MeetingEnded { app: String },

        // ── Recording lifecycle ───────────────────────────────────────────────
        /// Audio capture has begun.  Fired when [`MeetingListener::record`]
        /// successfully starts the system audio tap.
        RecordingStarted { app: String },

        /// Audio capture has stopped.  The WAV is being written; expect
        /// [`Event::RecordingReady`] shortly after.
        RecordingEnded { app: String },

        /// A completed WAV recording is available at `path`.
        /// Only fired when [`MeetingListener::record`] (or
        /// [`MeetingListener::auto_record`]) was active during the meeting.
        RecordingReady { path: std::path::PathBuf, app: String },

        // ── Capture health ────────────────────────────────────────────────────
        /// The audio or video capture stream was interrupted or resumed.
        /// For example, moving the meeting window to an inactive virtual desktop
        /// may interrupt capture.
        CaptureStatus { kind: CaptureKind, capturing: bool },

        // ── Errors ────────────────────────────────────────────────────────────
        /// An error occurred (e.g. the audio tap failed to start).
        Error { message: String },
    }

    // ── Supporting types ──────────────────────────────────────────────────────

    /// Which macOS system permission is being reported.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Permission {
        /// Microphone access — required to capture local mic audio.
        Microphone,
        /// Screen Recording — required for the system audio tap (macOS 14.2+).
        ScreenCapture,
        /// Accessibility — required by some meeting detection methods.
        Accessibility,
    }

    /// The current grant status of a permission.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PermissionGranted {
        /// Permission has been explicitly granted.
        Granted,
        /// The user has not yet been prompted (soft — the OS dialog will appear).
        NotRequested,
        /// The user explicitly denied the permission (hard failure).
        Denied,
    }

    /// Which media stream a [`CaptureStatus`] event refers to.
    ///
    /// [`CaptureStatus`]: Event::CaptureStatus
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CaptureKind {
        Audio,
        Video,
    }

    // ── Internal detection types (monitor ↔ recorder only) ───────────────────

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum DetectionKind { Started, Updated, Ended }

    #[derive(Debug, Clone)]
    pub(crate) struct Detection {
        pub(crate) kind:  DetectionKind,
        pub(crate) app:   String,
        /// Window title — set when kind == Updated.
        pub(crate) title: Option<String>,
    }

    // ── Internal audio types ──────────────────────────────────────────────────

    #[derive(Debug, Clone)]
    pub(crate) struct AudioChunk {
        pub(crate) pcm: Vec<i16>,
    }

    pub(crate) struct Recording {
        pub(crate) rx:      crossbeam_channel::Receiver<AudioChunk>,
        pub(crate) stop_fn: Option<Box<dyn FnOnce() + Send>>,
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            if let Some(f) = self.stop_fn.take() { f(); }
        }
    }

    // ── Errors ────────────────────────────────────────────────────────────────

    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        #[error("monitor already started")]
        AlreadyStarted,
        #[error("platform init failed: {0}")]
        PlatformInit(String),
        #[error("recording failed: {0}")]
        RecordingFailed(String),
        #[error("macOS 14.2+ required for system audio tap (running {major}.{minor})")]
        MacOSVersionTooOld { major: u32, minor: u32 },
        #[error("permission denied — check Screen Recording / Microphone in System Settings")]
        PermissionDenied,
        #[error("{0}")]
        Other(String),
    }

    pub type Result<T> = std::result::Result<T, Error>;
