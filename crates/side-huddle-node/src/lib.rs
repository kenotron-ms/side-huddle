#![deny(clippy::all)]

use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::{Env, JsFunction, JsObject};
use napi_derive::napi;

use side_huddle::{CaptureKind, Event, MeetingListener, Permission, PermissionGranted};

// ── Listener ──────────────────────────────────────────────────────────────────

#[napi(js_name = "Listener")]
pub struct JsListener {
    inner: MeetingListener,
}

#[napi]
impl JsListener {
    /// Create a new listener with default settings.
    #[napi(constructor)]
    pub fn new() -> Self {
        JsListener { inner: MeetingListener::new() }
    }

    /// Register an event handler.
    /// Multiple calls register multiple handlers; all are invoked in order.
    ///
    /// ```js
    /// listener.on((event) => {
    ///   if (event.kind === "MeetingDetected") listener.record();
    /// });
    /// ```
    #[napi]
    pub fn on(&self, _env: Env, callback: JsFunction) -> napi::Result<()> {
        let tsfn: ThreadsafeFunction<Event, ErrorStrategy::Fatal> =
            callback.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<Event>| {
                let obj = event_to_js(&ctx.env, &ctx.value)?;
                Ok(vec![obj.into_unknown()])
            })?;

        self.inner.on(move |event| {
            tsfn.call(event.clone(), ThreadsafeFunctionCallMode::NonBlocking);
        });

        Ok(())
    }

    /// Automatically record every detected meeting.
    #[napi]
    pub fn auto_record(&self) {
        self.inner.auto_record();
    }

    /// Start recording the current meeting.
    /// Call from within a MeetingDetected handler to opt in.
    #[napi]
    pub fn record(&self) {
        self.inner.record();
    }

    /// Set the PCM sample rate in Hz (default: 16000).
    /// Must be called before start().
    #[napi]
    pub fn set_sample_rate(&self, hz: u32) {
        self.inner.sample_rate(hz);
    }

    /// Set the WAV output directory (default: cwd).
    /// Must be called before start().
    #[napi]
    pub fn set_output_dir(&self, dir: String) {
        self.inner.output_dir(dir);
    }

    /// Start monitoring.  Events arrive via handlers registered with on().
    #[napi]
    pub fn start(&self) -> napi::Result<()> {
        self.inner
            .start()
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Stop monitoring and cancel any active recording.
    #[napi]
    pub fn stop(&self) {
        self.inner.stop();
    }
}

// ── Event → JS object conversion ─────────────────────────────────────────────

fn event_to_js(env: &Env, event: &Event) -> napi::Result<JsObject> {
    let mut obj = env.create_object()?;

    let kind = match event {
        Event::PermissionStatus { .. }  => "PermissionStatus",
        Event::PermissionsGranted       => "PermissionsGranted",
        Event::MeetingDetected { .. }   => "MeetingDetected",
        Event::MeetingUpdated { .. }    => "MeetingUpdated",
        Event::MeetingEnded { .. }      => "MeetingEnded",
        Event::RecordingStarted { .. }  => "RecordingStarted",
        Event::RecordingEnded { .. }    => "RecordingEnded",
        Event::RecordingReady { .. }    => "RecordingReady",
        Event::CaptureStatus { .. }     => "CaptureStatus",
        Event::Error { .. }             => "Error",
        Event::SpeakerChanged { .. }    => "SpeakerChanged",
    };
    obj.set_named_property("kind", env.create_string(kind)?)?;

    match event {
        Event::PermissionStatus { permission, status } => {
            obj.set_named_property("permission", env.create_string(match permission {
                Permission::Microphone    => "Microphone",
                Permission::ScreenCapture => "ScreenCapture",
                Permission::Accessibility => "Accessibility",
            })?)?;
            obj.set_named_property("status", env.create_string(match status {
                PermissionGranted::Granted      => "Granted",
                PermissionGranted::NotRequested => "NotRequested",
                PermissionGranted::Denied       => "Denied",
            })?)?;
        }
        Event::MeetingDetected { app, pid } => {
            obj.set_named_property("app", env.create_string(app)?)?;
            obj.set_named_property("pid", env.create_uint32(*pid)?)?;
        }
        Event::MeetingEnded { app }
        | Event::RecordingStarted { app }
        | Event::RecordingEnded { app } => {
            obj.set_named_property("app", env.create_string(app)?)?;
        }
        Event::MeetingUpdated { app, title } => {
            obj.set_named_property("app", env.create_string(app)?)?;
            obj.set_named_property("title", env.create_string(title)?)?;
        }
        Event::RecordingReady { mixed_path, others_path, self_path, app } => {
            obj.set_named_property("app", env.create_string(app)?)?;
            obj.set_named_property(
                "mixedPath",
                env.create_string(mixed_path.to_str().unwrap_or(""))?,
            )?;
            obj.set_named_property(
                "othersPath",
                env.create_string(others_path.to_str().unwrap_or(""))?,
            )?;
            obj.set_named_property(
                "selfPath",
                env.create_string(self_path.to_str().unwrap_or(""))?,
            )?;
        }
        Event::CaptureStatus { kind, capturing } => {
            obj.set_named_property("captureKind", env.create_string(match kind {
                CaptureKind::Audio => "Audio",
                CaptureKind::Video => "Video",
            })?)?;
            obj.set_named_property("capturing", env.get_boolean(*capturing)?)?;
        }
        Event::Error { message } => {
            obj.set_named_property("message", env.create_string(message)?)?;
        }
        Event::SpeakerChanged { speakers, app } => {
            obj.set_named_property("app", env.create_string(app)?)?;
            let mut arr = env.create_array_with_length(speakers.len())?;
            for (i, name) in speakers.iter().enumerate() {
                arr.set_element(i as u32, env.create_string(name)?)?;
            }
            obj.set_named_property("speakers", arr)?;
        }
        Event::PermissionsGranted => {}
    }

    Ok(obj)
}

/// Return the library version string.
#[napi]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
