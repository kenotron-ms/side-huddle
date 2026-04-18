    //! Window watcher — mirrors Go's `windowWatcher` in window_darwin.go exactly.
    //!
    //! After a meeting starts, the watcher runs a two-phase loop on a background
    //! thread:
    //!
    //! **Phase 1 — searching** (every 3 s):
    //!   Call `find_primary_window(owner)`. When it returns `Some((id, title))`
    //!   the specific window has been identified and the loop moves to Phase 2.
    //!
    //! **Phase 2 — watching** (every 3 s):
    //!   Call `window_exists(id)`. The moment it returns `false` — the call window
    //!   has been closed — `on_closed` is invoked exactly once and the thread exits.
    //!   This fires `MeetingEnded` immediately without waiting for the 20-second
    //!   audio grace period.
    //!
    //! The watcher is stopped (and the thread is signalled to exit) by dropping
    //! the `WindowWatcher` or calling `stop()`.

    use std::thread;
    use std::time::Duration;

    use super::window::{find_primary_window, window_exists};

    /// Poll interval: every 3 seconds, matching Go's `windowPollInterval`.
    const WINDOW_POLL_INTERVAL: Duration = Duration::from_secs(3);

    /// A running window-close watcher.  Dropping this value (or calling `stop()`)
    /// signals the background thread to exit cleanly.
    pub(crate) struct WindowWatcher {
        stop_tx: std::sync::mpsc::SyncSender<()>,
    }

    impl WindowWatcher {
        /// Spawn a window watcher for `owner` (a `CGWindowOwnerName` substring,
        /// e.g. `"Microsoft Teams"`).  `on_closed` is called at most once when
        /// the identified window disappears from the window list.
        /// Spawn a window watcher for `owner` (a `CGWindowOwnerName` substring,
        /// e.g. `"Microsoft Teams"`).
        ///
        /// - `on_identified(title)` — called once in Phase 1 when the call window
        ///   is found.  Fires `MeetingUpdated` with the window title.
        /// - `on_closed` — called once in Phase 2 when the identified window
        ///   disappears.  Fires `MeetingEnded` without waiting for audio to stop.
        pub(crate) fn start(
            owner:         String,
            on_closed:     impl Fn()       + Send + 'static,
            on_identified: impl Fn(String) + Send + 'static,
        ) -> Self {
            let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel::<()>(1);

            thread::spawn(move || {
                let mut watch_id: Option<u32> = None;

                loop {
                    match stop_rx.recv_timeout(WINDOW_POLL_INTERVAL) {
                        Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }

                    match watch_id {
                        None => {
                            // Phase 1 — find the call window; fire on_identified with its title.
                            if let Some((id, title)) = find_primary_window(&owner) {
                                on_identified(title);
                                watch_id = Some(id);
                            }
                        }
                        Some(id) => {
                            // Phase 2 — the window has closed; fire on_closed.
                            if !window_exists(id) {
                                on_closed();
                                return;
                            }
                        }
                    }
                }
            });

            WindowWatcher { stop_tx }
        }

    }

    /// Dropping a `WindowWatcher` automatically sends the stop signal so the
    /// background thread exits cleanly rather than continuing to poll.
    impl Drop for WindowWatcher {
        fn drop(&mut self) {
            let _ = self.stop_tx.try_send(());
        }
    }
