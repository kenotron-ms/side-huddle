# C usage guide

The C binding exposes the full side-huddle API through a plain C header. Any language that can call into a shared or static library can use it — C, C++, Zig, Swift, D, and so on.

## Prerequisites

- **macOS** 14.2+ for recording; detection works on earlier versions
- The compiled Rust library (`libside_huddle.dylib` or `libside_huddle.a`)
- The `include/side_huddle.h` header

Build the library first:

```bash
cargo build --release            # debug: target/debug/
# release: target/release/libside_huddle.dylib  (and .a)
```

### Linker flags — dynamic library

```bash
clang -o my_app my_app.c \
    -I include \
    -L target/release -lside_huddle \
    -Wl,-rpath,@executable_path/../target/release
```

### Linker flags — static library

Link the `.a` archive plus the macOS frameworks the library depends on:

```bash
clang -o my_app my_app.c \
    -I include \
    target/release/libside_huddle.a \
    -framework CoreAudio \
    -framework CoreGraphics \
    -framework CoreFoundation \
    -framework AVFoundation \
    -framework ScreenCaptureKit
```

---

## Basic example — opt-in recording

```c
#include <stdio.h>
#include <string.h>
#include "side_huddle.h"

static void on_event(const SHEvent *e, void *ctx)
{
    SideHuddleHandle h = (SideHuddleHandle)ctx;

    switch (e->kind) {
    case SH_MEETING_DETECTED:
        printf("meeting detected: %s\n", e->app);
        side_huddle_record(h); /* opt in to recording this meeting */
        break;
    case SH_RECORDING_READY:
        printf("saved: %s\n", e->path); /* copy if you need it later */
        break;
    case SH_MEETING_ENDED:
        printf("meeting ended\n");
        break;
    case SH_ERROR:
        fprintf(stderr, "error: %s\n", e->message);
        break;
    default:
        break;
    }
}

int main(void)
{
    SideHuddleHandle h = side_huddle_new();

    side_huddle_set_sample_rate(h, 16000);
    side_huddle_set_output_dir(h, "./recordings");
    side_huddle_on(h, on_event, h); /* pass handle as userdata */

    if (side_huddle_start(h) != 0) {
        fprintf(stderr, "start failed\n");
        side_huddle_free(h);
        return 1;
    }

    pause(); /* block until SIGINT */

    side_huddle_stop(h);
    side_huddle_free(h);
    return 0;
}
```

## Auto-record variant

Record every meeting automatically — no per-meeting opt-in:

```c
SideHuddleHandle h = side_huddle_new();
side_huddle_auto_record(h);

side_huddle_on(h, on_event, NULL);
side_huddle_start(h);
```

---

## API reference

### `SideHuddleHandle`

```c
typedef void* SideHuddleHandle;
```

An opaque pointer to a listener instance. Create one with `side_huddle_new()` and always free it with `side_huddle_free()` when done.

---

### `SHEventCallback`

```c
typedef void (*SHEventCallback)(const SHEvent *event, void *userdata);
```

Your event handler. Called on a **background thread** for every event. The `event` pointer and all string fields inside it are valid **only for the duration of this call** — copy any strings you need before returning. `userdata` is whatever you passed to `side_huddle_on()`.

---

### Functions

| Function | Returns | Description |
|---|---|---|
| `side_huddle_new()` | `SideHuddleHandle` | Allocate a new listener with default settings (16 kHz, cwd) |
| `side_huddle_free(h)` | `void` | Free the listener. Safe to call on `NULL` |
| `side_huddle_on(h, cb, userdata)` | `void` | Register an event callback. Call multiple times to add multiple handlers; all fire in registration order |
| `side_huddle_auto_record(h)` | `void` | Record every detected meeting automatically |
| `side_huddle_record(h)` | `void` | Opt in to recording the current meeting. No-op if no meeting is active or recording is already running. Call from within `SH_MEETING_DETECTED` |
| `side_huddle_set_sample_rate(h, hz)` | `void` | Set PCM sample rate in Hz (default: `16000`). Call before `side_huddle_start()` |
| `side_huddle_set_output_dir(h, dir)` | `void` | Set WAV output directory (default: cwd). Call before `side_huddle_start()` |
| `side_huddle_start(h)` | `int` | Begin monitoring. Returns `0` on success, `-1` on failure |
| `side_huddle_stop(h)` | `void` | Stop monitoring and cancel any active recording |
| `side_huddle_version()` | `const char*` | Library version string (static — do not free) |

---

### `SHEvent`

```c
typedef struct SHEvent {
    SHEventKind        kind;

    /* String fields — valid only during the callback */
    const char*        app;          /* Meeting app name                            */
    const char*        title;        /* Window title (SH_MEETING_UPDATED only)      */
    const char*        path;         /* Mixed WAV path (SH_RECORDING_READY)         */
    const char*        others_path;  /* Tap-only WAV path (SH_RECORDING_READY)      */
    const char*        self_path;    /* Mic-only WAV path (SH_RECORDING_READY)      */
    const char*        message;      /* Error description (SH_ERROR only)           */
    const char*        participant;  /* Tab-separated speaker names (SH_SPEAKER_CHANGED); "" = silence */

    /* SH_PERMISSION_STATUS fields */
    SHPermission       permission;
    SHPermissionStatus perm_status;

    /* SH_CAPTURE_STATUS fields */
    SHCaptureKind      capture_kind;
    int                capturing;    /* 1 = capturing, 0 = interrupted              */
} SHEvent;
```

Check `kind` first, then read the fields relevant to that event kind. Fields that do not apply to a given event are `NULL` (strings) or `0` (integers).

#### Field availability by event kind

| Field | Type | Set on |
|---|---|---|
| `kind` | `SHEventKind` | All events |
| `app` | `const char*` | `SH_MEETING_DETECTED`, `SH_MEETING_UPDATED`, `SH_MEETING_ENDED`, `SH_RECORDING_STARTED`, `SH_RECORDING_ENDED`, `SH_RECORDING_READY`, `SH_SPEAKER_CHANGED` |
| `title` | `const char*` | `SH_MEETING_UPDATED` |
| `path` | `const char*` | `SH_RECORDING_READY` — tap + mic combined (full meeting audio) |
| `others_path` | `const char*` | `SH_RECORDING_READY` — system tap only (other participants) |
| `self_path` | `const char*` | `SH_RECORDING_READY` — microphone only (local user) |
| `message` | `const char*` | `SH_ERROR` |
| `participant` | `const char*` | `SH_SPEAKER_CHANGED` — tab-separated names; `""` means silence |
| `permission` | `SHPermission` | `SH_PERMISSION_STATUS` |
| `perm_status` | `SHPermissionStatus` | `SH_PERMISSION_STATUS` |
| `capture_kind` | `SHCaptureKind` | `SH_CAPTURE_STATUS` |
| `capturing` | `int` | `SH_CAPTURE_STATUS` — `1` = active, `0` = interrupted |

---

### `SHEventKind`

Integer constants identifying the event type. Check `event->kind` in your callback.

| Constant | Value | Fires when |
|---|---|---|
| `SH_PERMISSION_STATUS` | `0` | Each macOS permission is checked on `side_huddle_start()`. Not emitted on Windows / Linux |
| `SH_PERMISSIONS_GRANTED` | `1` | All required permissions are OK. Emitted immediately on non-macOS platforms |
| `SH_MEETING_DETECTED` | `2` | Meeting mic activity sustained for 2 seconds |
| `SH_MEETING_UPDATED` | `3` | Window title identified — `app` and `title` are set |
| `SH_MEETING_ENDED` | `4` | Meeting stopped (window closed or mic went silent) |
| `SH_RECORDING_STARTED` | `5` | Audio capture began |
| `SH_RECORDING_ENDED` | `6` | Capture stopped; WAV is being written |
| `SH_RECORDING_READY` | `7` | Three WAV files written to disk |
| `SH_CAPTURE_STATUS` | `8` | Audio or video capture was interrupted or resumed |
| `SH_ERROR` | `9` | Something went wrong |
| `SH_SPEAKER_CHANGED` | `10` | Set of visually detected speaking participants changed. macOS only |

---

### `SHPermission`

Which macOS system permission is reported in an `SH_PERMISSION_STATUS` event.

| Constant | Value | Meaning |
|---|---|---|
| `SH_PERMISSION_MICROPHONE` | `0` | Microphone access — required to capture local mic audio |
| `SH_PERMISSION_SCREEN_CAPTURE` | `1` | Screen Recording — required for the system audio tap (macOS 14.2+) |
| `SH_PERMISSION_ACCESSIBILITY` | `2` | Accessibility — required by some meeting detection methods |

---

### `SHPermissionStatus`

Current grant status of a permission.

| Constant | Value | Meaning |
|---|---|---|
| `SH_PERM_GRANTED` | `0` | Permission has been explicitly granted |
| `SH_PERM_NOT_REQUESTED` | `1` | User has not yet been prompted — OS dialog will appear on first use |
| `SH_PERM_DENIED` | `2` | User explicitly denied the permission (hard failure) |

---

### `SHCaptureKind`

Which media stream a `SH_CAPTURE_STATUS` event refers to.

| Constant | Value | Meaning |
|---|---|---|
| `SH_CAPTURE_AUDIO` | `0` | Audio capture stream |
| `SH_CAPTURE_VIDEO` | `1` | Video capture stream |

---

## Memory rules

> **String pointers inside `SHEvent` are only valid for the duration of the callback. Copy them before returning.**

```c
static void on_event(const SHEvent *e, void *ctx)
{
    if (e->kind == SH_RECORDING_READY) {
        /* e->path is about to be freed — copy it now */
        char path_copy[1024];
        strncpy(path_copy, e->path, sizeof(path_copy) - 1);
        path_copy[sizeof(path_copy) - 1] = '\0';

        /* safe to use path_copy after the callback returns */
        enqueue_transcode_job(path_copy);
    }
}
```

Other memory rules:
- `SideHuddleHandle` is heap-allocated by `side_huddle_new()`. Always free it with `side_huddle_free()`.
- `userdata` is never touched by the library — thread-safety is entirely your responsibility.
- The string returned by `side_huddle_version()` is static — do **not** free it.

---

## Build example

Compile the quick-start example against the release dylib:

```bash
# 1. Build the Rust library
cargo build --release

# 2. Compile your C program
clang -o demo demo.c \
    -I include \
    -L target/release -lside_huddle \
    -Wl,-rpath,@executable_path/../../target/release

# 3. Run
./demo
```

Or against the static archive (no rpath needed, but requires framework flags):

```bash
clang -o demo demo.c \
    -I include \
    target/release/libside_huddle.a \
    -framework CoreAudio \
    -framework CoreGraphics \
    -framework CoreFoundation \
    -framework AVFoundation \
    -framework ScreenCaptureKit
```

---

## Handling all events

```c
#include <stdio.h>
#include "side_huddle.h"

static void on_event(const SHEvent *e, void *ctx)
{
    SideHuddleHandle h = (SideHuddleHandle)ctx;

    switch (e->kind) {
    case SH_PERMISSION_STATUS: {
        const char *perm = "unknown";
        switch (e->permission) {
            case SH_PERMISSION_MICROPHONE:     perm = "Microphone";     break;
            case SH_PERMISSION_SCREEN_CAPTURE: perm = "ScreenCapture";  break;
            case SH_PERMISSION_ACCESSIBILITY:  perm = "Accessibility";  break;
        }
        const char *st = "unknown";
        switch (e->perm_status) {
            case SH_PERM_GRANTED:       st = "granted";       break;
            case SH_PERM_NOT_REQUESTED: st = "not requested"; break;
            case SH_PERM_DENIED:        st = "denied";        break;
        }
        printf("permission %s: %s\n", perm, st);
        break;
    }
    case SH_PERMISSIONS_GRANTED:
        printf("all permissions granted\n");
        break;
    case SH_MEETING_DETECTED:
        printf("meeting detected: %s\n", e->app);
        side_huddle_record(h); /* opt in */
        break;
    case SH_MEETING_UPDATED:
        printf("meeting updated: %s — %s\n", e->app, e->title);
        break;
    case SH_MEETING_ENDED:
        printf("meeting ended: %s\n", e->app);
        break;
    case SH_RECORDING_STARTED:
        printf("recording started: %s\n", e->app);
        break;
    case SH_RECORDING_ENDED:
        printf("recording ended: %s\n", e->app);
        break;
    case SH_RECORDING_READY:
        printf("WAV files ready (%s):\n", e->app);
        printf("  mixed:  %s\n", e->path);
        printf("  others: %s\n", e->others_path);
        printf("  self:   %s\n", e->self_path);
        break;
    case SH_CAPTURE_STATUS:
        printf("capture %s: capturing=%d\n",
            e->capture_kind == SH_CAPTURE_AUDIO ? "audio" : "video",
            e->capturing);
        break;
    case SH_ERROR:
        fprintf(stderr, "error: %s\n", e->message);
        break;
    case SH_SPEAKER_CHANGED:
        printf("speakers (%s): %s\n",
            e->app,
            e->participant[0] ? e->participant : "(silence)");
        break;
    }
}

int main(void)
{
    SideHuddleHandle h = side_huddle_new();
    side_huddle_set_sample_rate(h, 16000);
    side_huddle_set_output_dir(h, "./recordings");
    side_huddle_on(h, on_event, h);

    if (side_huddle_start(h) != 0) {
        fprintf(stderr, "start failed\n");
        side_huddle_free(h);
        return 1;
    }

    pause();

    side_huddle_stop(h);
    side_huddle_free(h);
    return 0;
}
```

---

## Configuration

| Function | Default | Description |
|---|---|---|
| `side_huddle_set_sample_rate(h, hz)` | `16000` | Sample rate for WAV output |
| `side_huddle_set_output_dir(h, dir)` | Current directory | Directory where WAV files are written |
| `side_huddle_auto_record(h)` | Off | Record every meeting automatically |

Call configuration functions before `side_huddle_start()`.

---

## Notes

- `side_huddle_stop()` cancels any active recording and tears down the monitor. Always call it before `side_huddle_free()`.
- `side_huddle_record()` is a no-op if no meeting is active or if recording has already started.
- WAV output is mono 16-bit PCM at the configured sample rate. `SH_RECORDING_READY` delivers three files: mixed (tap + mic), others (tap only), and self (mic only).
- `SH_SPEAKER_CHANGED` — `participant` is a tab-separated list of speaker names. An empty string means silence or no chromatic ring was detected. macOS only.
- The callback fires on a background thread. If `userdata` is shared with other threads, protect it with a mutex.

## Platform notes

| Feature | macOS | Windows | Linux |
|---|---|---|---|
| Meeting detection | ✓ | ✓ (stub) | ✓ (stub) |
| Window title (`SH_MEETING_UPDATED`) | ✓ | — | — |
| Recording (`side_huddle_record` / `side_huddle_auto_record`) | ✓ macOS 14.2+ | — | — |
| Speaker diarization (`SH_SPEAKER_CHANGED`) | ✓ | — | — |
| Permission events (`SH_PERMISSION_STATUS`) | ✓ | — | — |
