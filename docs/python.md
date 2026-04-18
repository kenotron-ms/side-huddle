# Python usage guide

Python bindings for side-huddle use ctypes to call the Rust cdylib directly. No compilation or native Python extension is needed.

## Prerequisites

- **Python** 3.9+
- **Rust** 1.78+ (to build the cdylib)
- macOS 14.2+ for recording; detection works on earlier versions

### Build the shared library

```bash
cargo build --release
```

This produces `target/release/libside_huddle.dylib`, which the Python bindings load at runtime.

### Run the demo

```bash
make run-demo-python
```

## Basic example — opt-in recording

```python
from sidehuddle import Listener, EventKind

listener = Listener()

@listener.on
def handle(event):
    if event.kind == EventKind.MEETING_DETECTED:
        print(f"meeting detected: {event.app}")
        listener.record()  # opt in to recording this meeting
    elif event.kind == EventKind.RECORDING_READY:
        print(f"saved: {event.path}")
    elif event.kind == EventKind.MEETING_ENDED:
        print("meeting ended")
    elif event.kind == EventKind.ERROR:
        print(f"error: {event.message}")

listener.set_sample_rate(16000)
listener.set_output_dir("./recordings")

with listener:
    import signal
    signal.pause()  # block until interrupted
```

The `with` statement calls `listener.start()` on entry and `listener.stop()` on exit.

## Auto-record variant

Record every meeting automatically:

```python
from sidehuddle import Listener, EventKind

listener = Listener()
listener.auto_record()

@listener.on
def handle(event):
    if event.kind == EventKind.MEETING_DETECTED:
        print(f"meeting detected: {event.app}")
    elif event.kind == EventKind.RECORDING_READY:
        print(f"saved: {event.path}")

with listener:
    import signal
    signal.pause()
```

## Event reference

### Event kinds

| Constant | Fires when |
|---|---|
| `EventKind.PERMISSION_STATUS` | Each macOS permission is checked on `start()` |
| `EventKind.PERMISSIONS_GRANTED` | All required permissions are OK (immediate on Windows/Linux) |
| `EventKind.MEETING_DETECTED` | Meeting mic activity sustained for 2 seconds |
| `EventKind.MEETING_UPDATED` | Window title identified — includes app name and title |
| `EventKind.MEETING_ENDED` | Meeting stopped (window closed or mic went silent) |
| `EventKind.RECORDING_STARTED` | Audio capture began |
| `EventKind.RECORDING_ENDED` | Capture stopped, WAV is being finalized |
| `EventKind.RECORDING_READY` | WAV file written to disk |
| `EventKind.CAPTURE_STATUS` | Audio or video capture was interrupted or resumed |
| `EventKind.ERROR` | Something went wrong |

### Event fields

| Field | Type | Present on |
|---|---|---|
| `event.kind` | `EventKind` | All events |
| `event.app` | `str` | `MEETING_DETECTED`, `MEETING_UPDATED`, `MEETING_ENDED` |
| `event.title` | `str` | `MEETING_UPDATED` |
| `event.path` | `str` | `RECORDING_READY` |
| `event.message` | `str` | `ERROR` |
| `event.permission` | `Permission` | `PERMISSION_STATUS` |
| `event.perm_status` | `PermStatus` | `PERMISSION_STATUS` |
| `event.capture_kind` | `str` | `CAPTURE_STATUS` |
| `event.capturing` | `bool` | `CAPTURE_STATUS` |

### Permission constants

| Constant | Description |
|---|---|
| `Permission.MICROPHONE` | Microphone access |
| `Permission.SCREEN_CAPTURE` | Screen recording / system audio tap |
| `Permission.ACCESSIBILITY` | Accessibility API access |

### Permission status constants

| Constant | Description |
|---|---|
| `PermStatus.GRANTED` | Permission is granted |
| `PermStatus.NOT_REQUESTED` | Permission has not been requested yet |
| `PermStatus.DENIED` | Permission was denied |

## Configuration

| Method | Default | Description |
|---|---|---|
| `listener.set_sample_rate(hz)` | `16000` | Sample rate for WAV output |
| `listener.set_output_dir(path)` | Current directory | Directory where WAV files are written |
| `listener.auto_record()` | Off | Record every meeting automatically |

## Handling all events

```python
from sidehuddle import Listener, EventKind, Permission, PermStatus

listener = Listener()

@listener.on
def handle(event):
    match event.kind:
        case EventKind.PERMISSION_STATUS:
            print(f"permission {event.permission}: {event.perm_status}")
        case EventKind.PERMISSIONS_GRANTED:
            print("all permissions granted")
        case EventKind.MEETING_DETECTED:
            print(f"meeting detected: {event.app}")
        case EventKind.MEETING_UPDATED:
            print(f"meeting updated: {event.app} — {event.title}")
        case EventKind.MEETING_ENDED:
            print("meeting ended")
        case EventKind.RECORDING_STARTED:
            print("recording started")
        case EventKind.RECORDING_ENDED:
            print("recording ended")
        case EventKind.RECORDING_READY:
            print(f"WAV saved: {event.path}")
        case EventKind.CAPTURE_STATUS:
            print(f"capture {event.capture_kind}: capturing={event.capturing}")
        case EventKind.ERROR:
            print(f"error: {event.message}")
```

## Multiple handlers

Register more than one handler. All fire for every event:

```python
@listener.on
def log_all(event):
    print(f"[{event.kind}] {event}")

@listener.on
def record_logic(event):
    if event.kind == EventKind.MEETING_DETECTED:
        listener.record()
```

## Notes

- The Python bindings are pure ctypes — no Cython, no cffi, no build step beyond `cargo build --release`.
- The shared library is expected at `target/release/libside_huddle.dylib`. If you move it, set the path before importing.
- `listener.record()` is a no-op if no meeting is active or recording has already started.
- WAV output is mono 16-bit PCM at the configured sample rate.
- The `with listener:` context manager is the recommended pattern — it ensures `stop()` is called on exit or exception.
