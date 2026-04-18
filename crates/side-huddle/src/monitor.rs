    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use std::thread;

    use crate::{Detection, DetectionKind, Error, Result};
    use crate::platform;

    const SUSTAIN_DURATION:   Duration = Duration::from_secs(2);
    const END_GRACE_DURATION: Duration = Duration::from_secs(20);
    const POLL_INTERVAL:      Duration = Duration::from_millis(300);

    pub(crate) struct Monitor {
        inner: Arc<Mutex<Inner>>,
    }

    struct Inner {
        started:     bool,
        in_meeting:  bool,
        current_app: String,
        handlers:    Vec<Arc<dyn Fn(Detection) + Send + Sync + 'static>>,
        start_timer_cancel: Option<std::sync::mpsc::SyncSender<()>>,
        end_timer_cancel:   Option<std::sync::mpsc::SyncSender<()>>,
        #[cfg(target_os = "macos")]
        win_watcher: Option<platform::darwin::window_watcher::WindowWatcher>,
    }

    impl Monitor {
        pub(crate) fn new() -> Self {
            Monitor {
                inner: Arc::new(Mutex::new(Inner {
                    started:            false,
                    in_meeting:         false,
                    current_app:        String::new(),
                    handlers:           Vec::new(),
                    start_timer_cancel: None,
                    end_timer_cancel:   None,
                    #[cfg(target_os = "macos")]
                    win_watcher:        None,
                })),
            }
        }

        pub(crate) fn on_detection<F>(&mut self, f: F)
        where
            F: Fn(Detection) + Send + Sync + 'static,
        {
            self.inner.lock().unwrap().handlers.push(Arc::new(f));
        }

        pub(crate) fn start(&self) -> Result<()> {
            let mut g = self.inner.lock().unwrap();
            if g.started { return Err(Error::AlreadyStarted); }
            g.started = true;
            drop(g);

            let inner = Arc::clone(&self.inner);
            thread::spawn(move || {
                let mut last_pid: u32 = 0;
                loop {
                    let (pid, app) = platform::poll_active();
                    if pid != last_pid {
                        last_pid = pid;
                        handle_pid_change(Arc::clone(&inner), pid, app);
                    }
                    thread::sleep(POLL_INTERVAL);
                    if !inner.lock().unwrap().started { break; }
                }
            });
            Ok(())
        }

        pub(crate) fn stop(&self) {
            let mut g = self.inner.lock().unwrap();
            g.started            = false;
            g.start_timer_cancel = None;
            g.end_timer_cancel   = None;
            #[cfg(target_os = "macos")]
            { g.win_watcher = None; }
        }
    }

    fn handle_pid_change(inner: Arc<Mutex<Inner>>, pid: u32, app: String) {
        let mut g = inner.lock().unwrap();

        if pid != 0 {
            g.end_timer_cancel = None;
            if !g.in_meeting && g.start_timer_cancel.is_none() {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                g.start_timer_cancel = Some(tx);
                let inner2 = Arc::clone(&inner);
                let app2   = app.clone();
                thread::spawn(move || {
                    if rx.recv_timeout(SUSTAIN_DURATION).is_err() {
                        fire_started(inner2, app2);
                    }
                });
            }
            return;
        }

        g.start_timer_cancel = None;
        if g.in_meeting && g.end_timer_cancel.is_none() {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            g.end_timer_cancel = Some(tx);
            let inner2 = Arc::clone(&inner);
            thread::spawn(move || {
                if rx.recv_timeout(END_GRACE_DURATION).is_err() {
                    fire_ended(inner2);
                }
            });
        }
    }

    fn fire_started(inner: Arc<Mutex<Inner>>, app: String) {
        let mut g = inner.lock().unwrap();
        g.start_timer_cancel = None;
        if g.in_meeting { return; }
        g.in_meeting  = true;
        g.current_app = app.clone();

        #[cfg(target_os = "macos")]
        {
            use platform::darwin::window::cg_window_owner;
            use platform::darwin::window_watcher::WindowWatcher;

            let owner  = cg_window_owner(&app);
            let inner2 = Arc::clone(&inner);
            let inner3 = Arc::clone(&inner);
            let app3   = app.clone();

            g.win_watcher = Some(WindowWatcher::start(
                owner,
                // on_closed → fire MeetingEnded immediately
                move || fire_ended(Arc::clone(&inner2)),
                // on_identified → fire MeetingUpdated with window title
                move |title| fire_updated(Arc::clone(&inner3), app3.clone(), title),
            ));
        }

        let handlers = g.handlers.clone();
        drop(g);
        let det = Detection { kind: DetectionKind::Started, app, title: None };
        for h in &handlers { h(det.clone()); }
    }

    fn fire_updated(inner: Arc<Mutex<Inner>>, app: String, title: String) {
        let g = inner.lock().unwrap();
        if !g.in_meeting { return; }
        let handlers = g.handlers.clone();
        drop(g);
        let det = Detection { kind: DetectionKind::Updated, app, title: Some(title) };
        for h in &handlers { h(det.clone()); }
    }

    fn fire_ended(inner: Arc<Mutex<Inner>>) {
        let mut g = inner.lock().unwrap();
        g.end_timer_cancel = None;
        if !g.in_meeting { return; }
        let app = std::mem::take(&mut g.current_app);
        g.in_meeting = false;
        #[cfg(target_os = "macos")]
        { g.win_watcher = None; }

        let handlers = g.handlers.clone();
        drop(g);
        let det = Detection { kind: DetectionKind::Ended, app, title: None };
        for h in &handlers { h(det.clone()); }
    }
