# Go usage guide

Go bindings for side-huddle use CGo to link against the Rust cdylib.

## Prerequisites

- **Go** 1.22+
- **Rust** 1.78+ (to build the cdylib)
- macOS 14.2+ for recording; detection works on earlier versions

The simplest path is `make run-demo`, which builds the Rust library and runs the Go demo in one step.

To use the bindings in your own project:

```bash
go get github.com/kenotron-ms/side-huddle/bindings/go
```

## Basic example — opt-in recording

Listen for meetings and decide per-meeting whether to record:

```go
package main

import (
    "fmt"
    "log"

    sh "github.com/kenotron-ms/side-huddle/bindings/go"
)

func main() {
    listener := sh.New()

    listener.On(func(e *sh.Event) {
        switch e.Kind {
        case sh.MeetingDetected:
            fmt.Println("meeting detected:", e.App)
            listener.Record() // opt in to recording this meeting
        case sh.RecordingReady:
            fmt.Println("saved:", e.Path)
        case sh.MeetingEnded:
            fmt.Println("meeting ended")
        case sh.Error:
            fmt.Println("error:", e.Message)
        }
    })

    listener.SetSampleRate(16000)
    listener.SetOutputDir("./recordings")

    if err := listener.Start(); err != nil {
        log.Fatal(err)
    }
    defer listener.Stop()

    select {} // block forever
}
```

## Auto-record variant

Record every meeting automatically — no per-meeting opt-in needed:

```go
package main

import (
    "fmt"
    "log"

    sh "github.com/kenotron-ms/side-huddle/bindings/go"
)

func main() {
    listener := sh.New()
    listener.AutoRecord()

    listener.On(func(e *sh.Event) {
        switch e.Kind {
        case sh.MeetingDetected:
            fmt.Println("meeting detected:", e.App)
        case sh.RecordingReady:
            fmt.Println("saved:", e.Path)
        }
    })

    if err := listener.Start(); err != nil {
        log.Fatal(err)
    }
    defer listener.Stop()

    select {}
}
```

## Event reference

### Event kinds

| Constant | Fires when |
|---|---|
| `sh.PermissionStatus` | Each macOS permission is checked on `Start()` |
| `sh.PermissionsGranted` | All required permissions are OK (immediate on Windows/Linux) |
| `sh.MeetingDetected` | Meeting mic activity sustained for 2 seconds |
| `sh.MeetingUpdated` | Window title identified — includes app name and title |
| `sh.MeetingEnded` | Meeting stopped (window closed or mic went silent) |
| `sh.RecordingStarted` | Audio capture began |
| `sh.RecordingEnded` | Capture stopped, WAV is being finalized |
| `sh.RecordingReady` | WAV file written to disk |
| `sh.CaptureStatus` | Audio or video capture was interrupted or resumed |
| `sh.Error` | Something went wrong |

### Event fields

| Field | Type | Present on |
|---|---|---|
| `Kind` | `EventKind` | All events |
| `App` | `string` | `MeetingDetected`, `MeetingUpdated`, `MeetingEnded` |
| `Title` | `string` | `MeetingUpdated` |
| `Path` | `string` | `RecordingReady` |
| `Message` | `string` | `Error` |
| `Permission` | `string` | `PermissionStatus` |
| `PermStatus` | `string` | `PermissionStatus` |
| `CaptureKind` | `string` | `CaptureStatus` |
| `Capturing` | `bool` | `CaptureStatus` |

## Configuration

| Method | Default | Description |
|---|---|---|
| `listener.SetSampleRate(hz)` | `16000` | Sample rate for WAV output |
| `listener.SetOutputDir(path)` | Current directory | Directory where WAV files are written |
| `listener.AutoRecord()` | Off | Record every meeting automatically |

## Handling all events

```go
listener.On(func(e *sh.Event) {
    switch e.Kind {
    case sh.PermissionStatus:
        fmt.Printf("permission %s: %s\n", e.Permission, e.PermStatus)
    case sh.PermissionsGranted:
        fmt.Println("all permissions granted")
    case sh.MeetingDetected:
        fmt.Printf("meeting detected: %s\n", e.App)
    case sh.MeetingUpdated:
        fmt.Printf("meeting updated: %s — %s\n", e.App, e.Title)
    case sh.MeetingEnded:
        fmt.Println("meeting ended")
    case sh.RecordingStarted:
        fmt.Println("recording started")
    case sh.RecordingEnded:
        fmt.Println("recording ended")
    case sh.RecordingReady:
        fmt.Printf("WAV saved: %s\n", e.Path)
    case sh.CaptureStatus:
        fmt.Printf("capture %s: capturing=%v\n", e.CaptureKind, e.Capturing)
    case sh.Error:
        fmt.Printf("error: %s\n", e.Message)
    }
})
```

## Multiple handlers

You can register more than one handler. All fire for every event:

```go
// handler 1 — logging
listener.On(func(e *sh.Event) {
    log.Printf("[%s] %+v", e.Kind, e)
})

// handler 2 — recording logic
listener.On(func(e *sh.Event) {
    if e.Kind == sh.MeetingDetected {
        listener.Record()
    }
})
```

## Notes

- The Go bindings link against the Rust cdylib via CGo. The Makefile handles the build sequence — `make run-demo` is the easiest way to get started.
- `listener.Stop()` cancels any active recording and tears down the monitor. Always defer it.
- `listener.Record()` is a no-op if no meeting is active or if recording has already started.
- WAV output is mono 16-bit PCM at the configured sample rate.
