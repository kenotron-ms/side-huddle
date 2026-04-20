# side-huddle

Detect Teams, Zoom, and Google Meet meetings on your local machine and capture the audio as a WAV file. No cloud, no API keys, no bot joining the call — side-huddle runs invisibly using native OS audio APIs.

**Supported platforms:** macOS (full) · Windows (stub) · Linux (stub)

## Quick start

```bash
# Build everything + run the Go demo
make run-demo
```

Pick your language:

<table>
<tr><th>Go</th><th>Python</th><th>Node.js</th></tr>
<tr>
<td>

```go
listener := sh.New()
listener.On(func(e *sh.Event) {
    if e.Kind == sh.MeetingDetected {
        listener.Record()
    }
})
listener.Start()
```

</td>
<td>

```python
listener = Listener()

@listener.on
def _(event):
    if event.kind == EventKind.MEETING_DETECTED:
        listener.record()

listener.start()
```

</td>
<td>

```js
const listener = new Listener();
listener.on((event) => {
    if (event.kind === "MeetingDetected") {
        listener.record();
    }
});
listener.start();
```

</td>
</tr>
</table>

Full guides: [Go](docs/go.md) · [Python](docs/python.md) · [Node.js](docs/node.md) · [Rust](docs/rust.md) · [C/C++](docs/c.md)

## How it works

1. **Detection** — polls CoreAudio every 300 ms to find processes with active mic input matching known meeting apps (Teams, Zoom, Google Meet, browser-based). A 2-second sustain window avoids false positives.

2. **Window watcher** (macOS) — once a meeting is detected, monitors the meeting window via CoreGraphics. Fires `MeetingEnded` immediately when the window closes rather than waiting for the mic to go quiet.

3. **Recording** — uses a system audio tap (`CATapDescription`, macOS 14.2+) mixed with mic capture. Outputs a mono 16-bit PCM WAV file.

4. **Event emitter** — all lifecycle events fire to registered handlers. Multiple handlers per event are supported.

## Permissions (macOS)

| Permission | Required for | Grant via |
|---|---|---|
| **Screen Recording** | System audio tap (macOS 14.2+) | System Settings → Privacy & Security → Screen Recording |
| **Microphone** | Mic capture | System Settings → Privacy & Security → Microphone |

Detection alone requires no permissions.

## Event lifecycle

Events fire in this order for a recorded meeting:

```
PermissionStatus × N       per-permission status on start()
PermissionsGranted         all required permissions OK
MeetingDetected            meeting mic sustained for 2 s
MeetingUpdated             window title identified (app + title)
RecordingStarted           audio capture began
MeetingEnded               meeting stopped
RecordingEnded             capture stopped, WAV being written
RecordingReady             WAV file written to disk
```

Additional events: `CaptureStatus` (audio/video capture interrupted or resumed), `Error`.

## API

The API is the same across all three language bindings:

| Method | Description |
|---|---|
| `new Listener()` | Create a new listener instance |
| `listener.on(handler)` | Register an event handler (multiple allowed) |
| `listener.autoRecord()` | Record every detected meeting automatically |
| `listener.record()` | Opt in to recording the current meeting (call from `MeetingDetected`) |
| `listener.setSampleRate(hz)` | Set sample rate (default: 16 000) |
| `listener.setOutputDir(path)` | Set output directory for WAV files (default: cwd) |
| `listener.start()` | Begin monitoring for meetings |
| `listener.stop()` | Stop monitoring and cancel any active recording |
| `version()` | Library version string |

## Architecture

[`docs/architecture.dot`](docs/architecture.dot) is a Graphviz DOT file showing the full binding stack — user apps → language bindings → C ABI → Rust core → macOS platform layer → OS APIs.

Render it locally:

```bash
dot -Tsvg docs/architecture.dot -o docs/architecture.svg && open docs/architecture.svg
```

## Repository structure

```
crates/
  side-huddle/            Rust core library (rlib + cdylib)
  side-huddle-node/       napi-rs Node.js native addon
bindings/
  go/                     Go CGo bindings (wraps cdylib)
  python/                 Python ctypes bindings (wraps cdylib)
  node/                   Node.js demo
cmd/
  demo/                   Go demo
include/
  side_huddle.h           C header (for C/C++ consumers)
```

## Build requirements

| Dependency | Version | Notes |
|---|---|---|
| **Rust** | 1.78+ | Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin` |
| **Go** | 1.22+ | For Go bindings |
| **Node.js** | 18+ | For Node.js bindings |
| **Python** | 3.9+ | For Python bindings (pure ctypes, no compilation) |
| **macOS** | 14.2+ | Required for system audio tap; detection works on earlier versions |

## Makefile targets

```
make build               # Debug Rust build + verify Go
make release             # napi build --platform --release (also builds cdylib)
make run-demo            # Go demo
make run-demo-node       # Node.js demo
make run-demo-python     # Python demo
make clean
```

## License

See [LICENSE](LICENSE).
