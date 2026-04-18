# Node.js usage guide

Node.js bindings for side-huddle are built with napi-rs as a native addon.

## Prerequisites

- **Node.js** 18+
- **Rust** 1.78+ (to build the native addon)
- macOS 14.2+ for recording; detection works on earlier versions

### Build the native addon

```bash
cd crates/side-huddle-node
npx napi build --platform --release
```

This generates three files in `crates/side-huddle-node/`:
- `index.js` — cross-platform loader
- `index.d.ts` — TypeScript type definitions
- `side-huddle-node.darwin-arm64.node` (or equivalent for your platform)

### Run the demo

```bash
make run-demo-node
```

## Basic example — opt-in recording

```js
const { Listener, version } = require("../../crates/side-huddle-node");

const listener = new Listener();

listener.on((event) => {
    if (event.kind === "MeetingDetected") {
        console.log("meeting detected:", event.app);
        listener.record(); // opt in to recording this meeting
    }
    if (event.kind === "RecordingReady") {
        console.log("saved:", event.path);
    }
    if (event.kind === "MeetingEnded") {
        console.log("meeting ended");
    }
    if (event.kind === "Error") {
        console.error("error:", event.message);
    }
});

listener.setSampleRate(16000);
listener.setOutputDir("./recordings");

listener.start();

process.on("SIGINT", () => {
    listener.stop();
    process.exit(0);
});
```

## Auto-record variant

Record every meeting automatically:

```js
const { Listener } = require("../../crates/side-huddle-node");

const listener = new Listener();
listener.autoRecord();

listener.on((event) => {
    if (event.kind === "MeetingDetected") {
        console.log("meeting detected:", event.app);
    }
    if (event.kind === "RecordingReady") {
        console.log("saved:", event.path);
    }
});

listener.start();

process.on("SIGINT", () => {
    listener.stop();
    process.exit(0);
});
```

## Event reference

### Event kinds

Event kinds are plain strings in Node.js (no enum):

| Kind string | Fires when |
|---|---|
| `"PermissionStatus"` | Each macOS permission is checked on `start()` |
| `"PermissionsGranted"` | All required permissions are OK (immediate on Windows/Linux) |
| `"MeetingDetected"` | Meeting mic activity sustained for 2 seconds |
| `"MeetingUpdated"` | Window title identified — includes app name and title |
| `"MeetingEnded"` | Meeting stopped (window closed or mic went silent) |
| `"RecordingStarted"` | Audio capture began |
| `"RecordingEnded"` | Capture stopped, WAV is being finalized |
| `"RecordingReady"` | WAV file written to disk |
| `"CaptureStatus"` | Audio or video capture was interrupted or resumed |
| `"Error"` | Something went wrong |

### Event object shape

```ts
{
    kind: string;           // always present
    app?: string;           // MeetingDetected, MeetingUpdated, MeetingEnded
    title?: string;         // MeetingUpdated
    path?: string;          // RecordingReady
    message?: string;       // Error
    permission?: string;    // PermissionStatus
    status?: string;        // PermissionStatus
    captureKind?: string;   // CaptureStatus
    capturing?: boolean;    // CaptureStatus
}
```

Fields not relevant to the current event kind are `undefined`.

## Configuration

| Method | Default | Description |
|---|---|---|
| `listener.setSampleRate(hz)` | `16000` | Sample rate for WAV output |
| `listener.setOutputDir(path)` | Current directory | Directory where WAV files are written |
| `listener.autoRecord()` | Off | Record every meeting automatically |

## Handling all events

```js
listener.on((event) => {
    switch (event.kind) {
        case "PermissionStatus":
            console.log(`permission ${event.permission}: ${event.status}`);
            break;
        case "PermissionsGranted":
            console.log("all permissions granted");
            break;
        case "MeetingDetected":
            console.log("meeting detected:", event.app);
            break;
        case "MeetingUpdated":
            console.log(`meeting updated: ${event.app} — ${event.title}`);
            break;
        case "MeetingEnded":
            console.log("meeting ended");
            break;
        case "RecordingStarted":
            console.log("recording started");
            break;
        case "RecordingEnded":
            console.log("recording ended");
            break;
        case "RecordingReady":
            console.log("WAV saved:", event.path);
            break;
        case "CaptureStatus":
            console.log(`capture ${event.captureKind}: capturing=${event.capturing}`);
            break;
        case "Error":
            console.error("error:", event.message);
            break;
    }
});
```

## Multiple handlers

Register more than one handler. All fire for every event:

```js
// handler 1 — logging
listener.on((event) => {
    console.log(`[${event.kind}]`, event);
});

// handler 2 — recording logic
listener.on((event) => {
    if (event.kind === "MeetingDetected") {
        listener.record();
    }
});
```

## TypeScript

The napi-rs build generates `index.d.ts` with full type definitions. Import normally:

```ts
import { Listener, version } from "../../crates/side-huddle-node";
```

## Library version

```js
const { version } = require("../../crates/side-huddle-node");
console.log(version()); // e.g. "0.1.0"
```

## Notes

- `listener.stop()` cancels any active recording and tears down the monitor. Always wire it to `SIGINT`.
- `listener.record()` is a no-op if no meeting is active or recording has already started.
- WAV output is mono 16-bit PCM at the configured sample rate.
- The `.node` binary is platform-specific. Rebuild with `npx napi build --platform --release` when switching architectures.
