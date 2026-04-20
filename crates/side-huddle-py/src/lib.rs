//! PyO3 bindings for side-huddle.
//!
//! Compiled by maturin into a platform wheel and uploaded to PyPI.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use side_huddle::{CaptureKind, Event, MeetingListener, Permission, PermissionGranted};

// ── Event → Python dict ───────────────────────────────────────────────────────

fn event_to_py(py: Python<'_>, event: &Event) -> PyResult<PyObject> {
    let d = PyDict::new(py);

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
    d.set_item("kind", kind)?;

    match event {
        Event::PermissionStatus { permission, status } => {
            d.set_item("permission", match permission {
                Permission::Microphone    => "Microphone",
                Permission::ScreenCapture => "ScreenCapture",
                Permission::Accessibility => "Accessibility",
            })?;
            d.set_item("status", match status {
                PermissionGranted::Granted      => "Granted",
                PermissionGranted::NotRequested => "NotRequested",
                PermissionGranted::Denied       => "Denied",
            })?;
        }
        Event::MeetingDetected { app, pid } => {
            d.set_item("app", app.as_str())?;
            d.set_item("pid", *pid)?;
        }
        Event::MeetingEnded { app }
        | Event::RecordingStarted { app }
        | Event::RecordingEnded { app } => {
            d.set_item("app", app.as_str())?;
        }
        Event::MeetingUpdated { app, title } => {
            d.set_item("app", app.as_str())?;
            d.set_item("title", title.as_str())?;
        }
        Event::RecordingReady { mixed_path, others_path, self_path, app } => {
            d.set_item("app", app.as_str())?;
            d.set_item("mixed_path",  mixed_path.to_str().unwrap_or(""))?;
            d.set_item("others_path", others_path.to_str().unwrap_or(""))?;
            d.set_item("self_path",   self_path.to_str().unwrap_or(""))?;
        }
        Event::CaptureStatus { kind, capturing } => {
            d.set_item("capture_kind", match kind {
                CaptureKind::Audio => "Audio",
                CaptureKind::Video => "Video",
            })?;
            d.set_item("capturing", *capturing)?;
        }
        Event::Error { message } => {
            d.set_item("message", message.as_str())?;
        }
        Event::SpeakerChanged { speakers, app } => {
            d.set_item("app", app.as_str())?;
            d.set_item("speakers", speakers.clone())?;
        }
        Event::PermissionsGranted => {}
    }

    Ok(d.into())
}

// ── Listener class ────────────────────────────────────────────────────────────

/// Detect Teams / Zoom / Google Meet meetings and emit lifecycle events.
///
/// Usage::
///
///     from sidehuddle import Listener
///
///     listener = Listener()
///
///     @listener.on
///     def _(event):
///         if event["kind"] == "MeetingDetected":
///             listener.record()
///
///     with listener:
///         import signal; signal.pause()
#[pyclass]
struct Listener {
    inner: MeetingListener,
}

#[pymethods]
impl Listener {
    /// Create a new listener with default settings (16 kHz, cwd output directory).
    #[new]
    fn new() -> Self {
        Listener { inner: MeetingListener::new() }
    }

    /// Register an event handler.  Multiple calls register multiple handlers;
    /// all are invoked in order.  Can be used as a decorator.
    ///
    /// The handler receives a single dict with at least a ``kind`` key.
    fn on(&self, callback: PyObject) -> PyResult<()> {
        self.inner.on(move |event| {
            Python::with_gil(|py| {
                match event_to_py(py, event) {
                    Ok(ev) => { let _ = callback.call1(py, (ev,)); }
                    Err(e) => e.print(py),
                }
            });
        });
        Ok(())
    }

    /// Automatically record every detected meeting.
    fn auto_record(&self) {
        self.inner.auto_record();
    }

    /// Start recording the current meeting.
    /// Call from within a ``MeetingDetected`` handler to opt in.
    fn record(&self) {
        self.inner.record();
    }

    /// Open System Settings to grant the permissions required for recording.
    ///
    /// On macOS, Screen Recording cannot be requested via an inline dialog.
    /// This opens System Settings → Privacy & Security → Screen Recording so
    /// the user can grant access.  After granting, restart the listener.
    fn request_permissions(&self) {
        self.inner.request_permissions();
    }

    /// Set the PCM sample rate in Hz (default: 16000).
    /// Must be called before :meth:`start`.
    fn set_sample_rate(&self, hz: u32) {
        self.inner.sample_rate(hz);
    }

    /// Set the WAV output directory (default: cwd).
    /// Must be called before :meth:`start`.
    fn set_output_dir(&self, path: &str) {
        self.inner.output_dir(path);
    }

    /// Start monitoring.  Events arrive via registered handlers.
    fn start(&self) -> PyResult<()> {
        self.inner
            .start()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Stop monitoring and cancel any active recording.
    fn stop(&self) {
        self.inner.stop();
    }

    // ── Context manager ───────────────────────────────────────────────────────

    fn __enter__(slf: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
        slf.inner
            .start()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(slf)
    }

    fn __exit__(
        &self,
        _exc_type: PyObject,
        _exc_val: PyObject,
        _exc_tb: PyObject,
    ) -> bool {
        self.inner.stop();
        false
    }
}

// ── Module ────────────────────────────────────────────────────────────────────

#[pymodule]
fn sidehuddle(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Listener>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
