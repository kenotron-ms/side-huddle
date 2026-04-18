    //! Platform detection and recording dispatch.
    //! Each OS implements `poll_active()`, `start_tap()`, and `start_mic()`.

    use crate::{Recording, Result};

    // ── Poll ──────────────────────────────────────────────────────────────────────

    /// Returns (pid, friendly_app_name) of the first active meeting app,
    /// or (0, "") if no meeting is in progress.
    pub(crate) fn poll_active() -> (u32, String) {
        #[cfg(target_os = "macos")]
        { darwin::detect::poll_active() }
        #[cfg(target_os = "windows")]
        { windows::detect::poll_active() }
        #[cfg(target_os = "linux")]
        { linux::detect::poll_active() }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        { (0, String::new()) }
    }

    // ── Recording ─────────────────────────────────────────────────────────────────

    pub(crate) fn start_tap(sample_rate: u32, chunk_ms: u32) -> Result<Recording> {
        #[cfg(target_os = "macos")]
        { darwin::record_tap::start(sample_rate, chunk_ms) }
        #[cfg(not(target_os = "macos"))]
        { Err(crate::Error::Other("system audio tap not yet implemented on this platform".into())) }
    }

    pub(crate) fn start_mic(sample_rate: u32, chunk_ms: u32) -> Result<Recording> {
        #[cfg(target_os = "macos")]
        { darwin::record_mic::start(sample_rate, chunk_ms) }
        #[cfg(not(target_os = "macos"))]
        { Err(crate::Error::Other("mic recording not yet implemented on this platform".into())) }
    }

    // ── Platform modules ──────────────────────────────────────────────────────────

    #[cfg(target_os = "macos")]
    pub(crate) mod darwin;

    #[cfg(target_os = "windows")]
    pub(crate) mod windows;

    #[cfg(target_os = "linux")]
    pub(crate) mod linux;
    