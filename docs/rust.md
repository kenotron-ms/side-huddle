# Rust usage guide

The `side-huddle` crate is the native Rust library. All other language bindings wrap it, so you get the lowest overhead and the richest type system here.

## Prerequisites

- **Rust** 1.78+ (edition 2021)
- macOS 14.2+ for recording; detection works on earlier macOS versions
- Windows and Linux compile and detect meetings but cannot record (audio tap is macOS-only)

## Add the dependency

```toml
[dependencies]
side-huddle = { git = "https://github.com/kenotron-ms/side-huddle" }
```

## Basic example — opt-in recording

Listen for meetings and decide per-meeting whether to record:

```rust
use side_huddle::{Event, MeetingListener};

fn main() -> side_huddle::Result<()> {
    let listener = MeetingListener::new();

    // Clone to call listener methods from inside the handler.
    let l = listener.clone();
    listener.on(move |event| match event {
        Event::MeetingDetected { app, .. } => {
            println!("meeting detected: {app}");
            l.record(); // opt in to recording this meeting
        }
        Event::RecordingReady { mixed_path, .. } => {
            println!("saved: {}", mixed_path.display());
        }
        Event::MeetingEnded { .. } => println!("meeting ended"),
        Event::Error { message }   => eprintln!("error: {message}"),
        _ => {}
    });

    listener.sample_rate(16_000).output_dir("./recordings");

    listener.start()?;
    std::thread::park(); // block the main thread
    Ok(())
}
```

## Auto-record variant

Record every meeting automatically — no per-meeting opt-in needed:

```rust
use side_huddle::{Event, MeetingListener};

fn main() -> side_huddle::Result<()> {
    let listener = MeetingListener::new();
    listener.auto_record();

    listener.on(|event| match event {
        Event::MeetingDetected { app, .. } => println!("meeting detected: {app}"),
        Event::RecordingReady { mixed_path, .. } => {
            println!("saved: {}", mixed_path.display());
        }
        _ => {}
    });

    listener.start()?;
    std::thread::park();
    Ok(())
}
```

## API reference

### `MeetingListener`

The central type. Cheaply cloneable — all clones share the same internal state via `Arc`. Clone it before passing it into an `on` handler so you can call `record()` or `stop()` from inside the callback.

| Method | Returns | Description |
|---|---|---|
| `MeetingListener::new()` | `MeetingListener` | Create a listener with default settings (16 kHz, current directory) |
| `.sample_rate(hz: u32)` | `&Self` | Set WAV sample rate. Call before `start()` |
| `.output_dir(dir)` | `&Self` | Set WAV output directory. Call before `start()` |
| `.on(f)` | `&Self` | Register an event handler. Multiple handlers are all called in registration order |
| `.auto_record()` | `&Self` | Record every detected meeting automatically |
| `.record()` | `()` | Opt in to recording the current meeting. No-op if no meeting is active or recording is already running |
| `.start()` | `Result<()>` | Begin monitoring. Emits `PermissionStatus` × N then `PermissionsGranted` before the first detection event |
| `.stop()` | `()` | Stop monitoring and cancel any active recording |

`sample_rate`, `output_dir`, `on`, and `auto_record` all return `&Self` so they can be chained:

```rust
listener
    .sample_rate(48_000)
    .output_dir("/tmp/meetings")
    .auto_record();
```

---

### `Event`

All events emitted by `MeetingListener`. Register handlers with `listener.on(...)`.

```rust
#[derive(Debug, Clone)]
pub enum Event { ... }
```

#### Variant reference

| Variant | Fields | Fires when |
|---|---|---|
| `PermissionStatus` | `permission`, `status` | Each macOS permission is checked on `start()`. Not emitted on Windows / Linux |
| `PermissionsGranted` | — | All required permissions are OK. Emitted immediately on non-macOS platforms |
| `MeetingDetected` | `app`, `pid` | Meeting mic activity sustained for 2 seconds |
| `MeetingUpdated` | `app`, `title` | Window title identified via window scan |
| `MeetingEnded` | `app` | Meeting stopped (window closed or mic went silent) |
| `RecordingStarted` | `app` | Audio capture began |
| `RecordingEnded` | `app` | Capture stopped; WAV is being written |
| `RecordingReady` | `mixed_path`, `others_path`, `self_path`, `app` | Three WAV files written to disk |
| `CaptureStatus` | `kind`, `capturing` | An audio or video capture stream was interrupted or resumed |
| `Error` | `message` | Something went wrong (e.g. audio tap failed to start) |
| `SpeakerChanged` | `speakers`, `app` | The set of visually detected speaking participants changed. macOS only |

#### Field types

| Field | Type | Present on |
|---|---|---|
| `app` | `String` | `MeetingDetected`, `MeetingUpdated`, `MeetingEnded`, `RecordingStarted`, `RecordingEnded`, `RecordingReady`, `SpeakerChanged` |
| `pid` | `u32` | `MeetingDetected` |
| `title` | `String` | `MeetingUpdated` |
| `mixed_path` | `PathBuf` | `RecordingReady` — tap + mic combined (full meeting audio) |
| `others_path` | `PathBuf` | `RecordingReady` — system tap only (what other participants said) |
| `self_path` | `PathBuf` | `RecordingReady` — microphone only (what you said) |
| `message` | `String` | `Error` |
| `permission` | `Permission` | `PermissionStatus` |
| `status` | `PermissionGranted` | `PermissionStatus` |
| `kind` | `CaptureKind` | `CaptureStatus` |
| `capturing` | `bool` | `CaptureStatus` — `true` = active, `false` = interrupted |
| `speakers` | `Vec<String>` | `SpeakerChanged` — empty vec means silence / no speaker ring detected |

---

### `Permission`

Which macOS system permission is being reported in a `PermissionStatus` event.

```rust
pub enum Permission {
    Microphone,    // Required to capture local mic audio
    ScreenCapture, // Required for the system audio tap (macOS 14.2+)
    Accessibility, // Required by some meeting-detection methods
}
```

---

### `PermissionGranted`

The current grant status of a permission.

```rust
pub enum PermissionGranted {
    Granted,      // Permission has been explicitly granted
    NotRequested, // User has not yet been prompted — OS dialog will appear on first use
    Denied,       // User explicitly denied the permission (hard failure)
}
```

---

### `CaptureKind`

Which media stream a `CaptureStatus` event refers to.

```rust
pub enum CaptureKind {
    Audio,
    Video,
}
```

---

### `Error`

`side_huddle::Result<T>` is `std::result::Result<T, side_huddle::Error>`.

```rust
pub enum Error {
    AlreadyStarted,                          // start() called twice
    PlatformInit(String),                    // OS monitor failed to initialise
    RecordingFailed(String),                 // Audio tap or mic capture failed
    MacOSVersionTooOld { major, minor },     // macOS < 14.2 (recording only)
    PermissionDenied,                        // Screen Recording or Mic denied
    Other(String),
}
```

---

## Threading

The closure passed to `on` must be `Send + Sync + 'static`:

```rust
pub fn on<F: Fn(&Event) + Send + Sync + 'static>(&self, f: F) -> &Self
```

Handlers are called on background threads managed by the library. Use `Arc<Mutex<_>>` or channels if you need to share state with the rest of your program. `MeetingListener` itself is `Clone + Send + Sync`, so cloning it and moving it into a handler is always safe:

```rust
let listener = MeetingListener::new();

// handler 1 — logging (no clone needed; &Event is enough)
listener.on(|e| println!("{e:?}"));

// handler 2 — recording logic (needs a clone to call record())
let l = listener.clone();
listener.on(move |e| {
    if let Event::MeetingDetected { .. } = e {
        l.record();
    }
});
```

---

## Configuration

| Method | Default | Description |
|---|---|---|
| `.sample_rate(hz)` | `16000` | Sample rate for WAV output |
| `.output_dir(path)` | Current directory | Directory where WAV files are written |
| `.auto_record()` | Off | Record every meeting without a per-meeting opt-in |

Call configuration methods before `start()`. Calling them afterwards has no effect on an already-running monitor.

---

## Handling all events

```rust
use side_huddle::{CaptureKind, Event, MeetingListener, Permission, PermissionGranted};

fn main() -> side_huddle::Result<()> {
    let listener = MeetingListener::new();
    let l = listener.clone();

    listener.on(move |event| match event {
        Event::PermissionStatus { permission, status } => {
            let perm = match permission {
                Permission::Microphone    => "Microphone",
                Permission::ScreenCapture => "ScreenCapture",
                Permission::Accessibility => "Accessibility",
            };
            let st = match status {
                PermissionGranted::Granted      => "granted",
                PermissionGranted::NotRequested => "not requested",
                PermissionGranted::Denied       => "denied",
            };
            println!("permission {perm}: {st}");
        }
        Event::PermissionsGranted => {
            println!("all permissions granted");
        }
        Event::MeetingDetected { app, pid } => {
            println!("meeting detected: {app} (pid {pid})");
            l.record(); // opt in
        }
        Event::MeetingUpdated { app, title } => {
            println!("meeting updated: {app} — {title}");
        }
        Event::MeetingEnded { app } => {
            println!("meeting ended: {app}");
        }
        Event::RecordingStarted { app } => {
            println!("recording started: {app}");
        }
        Event::RecordingEnded { app } => {
            println!("recording ended: {app}");
        }
        Event::RecordingReady { mixed_path, others_path, self_path, app } => {
            println!("WAV files ready ({app}):");
            println!("  mixed:  {}", mixed_path.display());
            println!("  others: {}", others_path.display());
            println!("  self:   {}", self_path.display());
        }
        Event::CaptureStatus { kind, capturing } => {
            let k = match kind {
                CaptureKind::Audio => "audio",
                CaptureKind::Video => "video",
            };
            println!("capture {k}: capturing={capturing}");
        }
        Event::Error { message } => {
            eprintln!("error: {message}");
        }
        Event::SpeakerChanged { speakers, app } => {
            if speakers.is_empty() {
                println!("{app}: silence");
            } else {
                println!("{app} speakers: {}", speakers.join(", "));
            }
        }
    });

    listener.start()?;
    std::thread::park();
    Ok(())
}
```

---

## Notes

- `listener.stop()` cancels any active recording and tears down the monitor. Always call it (or `defer` / `Drop`) before your process exits.
- `listener.record()` is a no-op if no meeting is active or if a recording is already running.
- WAV output is mono 16-bit PCM at the configured sample rate. `RecordingReady` delivers three files: mixed (tap + mic), others (tap only), and self (mic only).
- `SpeakerChanged` uses visual detection of speaking-indicator rings and is macOS-only. It is never emitted on Windows or Linux.

## Platform notes

| Feature | macOS | Windows | Linux |
|---|---|---|---|
| Meeting detection | ✓ | ✓ (stub) | ✓ (stub) |
| Window title (`MeetingUpdated`) | ✓ | — | — |
| Recording (`record()` / `auto_record()`) | ✓ macOS 14.2+ | — | — |
| Speaker diarization (`SpeakerChanged`) | ✓ | — | — |
| Permission events | ✓ | — | — |
