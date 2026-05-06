# Recording HUD + ⌥Space Hotkey — Design Spec

**Date:** 2026-05-06  
**Status:** Approved

---

## Overview

Three improvements to the `cmd/side-huddle` macOS app:

1. **Persistent recording HUD** — a small floating pill (separate NSPanel) that shows while recording and while transcribing, replacing the current full-card recording/transcribing states in the existing overlay.
2. **⌥Space global hotkey** — toggle ad-hoc recording from anywhere, no meeting detection required.
3. **Auto-transcription** — transcription starts automatically on `RecordingReady`, no "Transcribe?" prompt.

Also fixes a latent bug: the "Stop Recording" menu-bar item is currently a no-op.

---

## 1. Recording HUD Panel

### New files

| File | Purpose |
|------|---------|
| `cmd/side-huddle/hud_darwin.go` | Go bridge — marshals state JSON → C calls; exported callbacks |
| `cmd/side-huddle/hud_darwin.m` | ObjC NSPanel implementation |
| `cmd/side-huddle/hud_other.go` | Build-tag stub for non-darwin |

### Panel spec

| Property | Value |
|----------|-------|
| Size | ~200 × 40 pt (pill shape) |
| Position | Top-right, 8 pt from screen edge, below menu bar |
| Level | `NSFloatingWindowLevel` |
| Style | Borderless, `NonactivatingPanel` |
| Background | Transparent (`clearColor`) with `backdrop-filter: blur` in the WKWebView |
| Collection behavior | `CanJoinAllSpaces` + `Stationary` + `IgnoresCycle` |

Driven by `setState(json)` via `evaluateJavaScript` — same pattern as the existing overlay.

### States

#### `recording`
```json
{ "type": "recording", "elapsed": 167 }
```
- Circle icon: `#30d158` (iOS green) fill tint + border, 3 animated waveform bars
- Right of icon: elapsed timer in `MM:SS`, `rgba(255,255,255,0.55)`
- Pill border: `rgba(48,209,88,0.22)`

#### `transcribing`
```json
{ "type": "transcribing" }
```
- Circle icon: `#0a84ff` (iOS blue) fill tint + border, spinning ring (14 pt, 0.75s)
- Right of icon: "Transcribing" label `rgba(255,255,255,0.7)`
- Pill border: `rgba(10,132,255,0.22)`

#### `error`
```json
{ "type": "error", "message": "whisper-cli not found" }
```
- Circle icon: `#ff453a` (iOS red), `✕` glyph
- Right of icon: two-line layout — "Transcription failed" (`#ff453a`, 11pt bold) / error message (`rgba(255,255,255,0.4)`, 10pt)
- Pill border: `rgba(255,69,58,0.35)`
- Persists until tapped/clicked; click sends `dismiss` action → `shHudAction("dismiss")`

#### Transitions
- `recording` → `transcribing`: cross-fade on `RecordingReady`
- `transcribing` → hidden: slide right + fade when transcription succeeds
- `transcribing` → `error`: cross-fade on transcription failure

### Go API (hud_darwin.go)

```go
func hudRecording()               // show recording state, start elapsed counter
func hudTranscribing()            // show transcribing state
func hudError(msg string)         // show error state with message
func hudHide()                    // slide out and hide
func hudWarmup()                  // pre-create panel at launch (avoids first-show lag)
```

Exported callback to ObjC:
```go
//export shHudAction
func shHudAction(action *C.char)  // "dismiss" from error state click
```

---

## 2. ⌥Space Global Hotkey

### Wiring

`CGEventTap` installed in `sh_cocoa_activate()` in `cocoa_darwin.m`.

- **Event type:** `kCGEventKeyDown`
- **Key code:** 49 (space)
- **Modifier:** `kCGEventFlagMaskAlternate` (Option key)
- **Action:** swallow the event (return `NULL`), call `goHotkeyCallback()`

```objc
// In sh_cocoa_activate():
CGEventMask mask = CGEventMaskBit(kCGEventKeyDown);
CFMachPortRef tap = CGEventTapCreate(
    kCGSessionEventTap,
    kCGHeadInsertEventTap,
    kCGEventTapOptionDefault,
    mask,
    shHotkeyCallback,
    NULL
);
```

### Accessibility permission

`CGEventTap` requires the Accessibility TCC permission (`com.apple.security.accessibility`).

On first ⌥Space press without permission:
1. `CGEventTap` callback is nil / tap is disabled.
2. `sh_cocoa_activate` checks `AXIsProcessTrusted()` at launch; if false, adds a "Grant Accessibility Access" menu item.
3. Clicking the menu item calls `AXIsProcessTrustedWithOptions` with the prompt dict to open System Settings.

### Toggle behavior (Go side)

New file `hotkey_darwin.go` (separate file required — CGo prohibits mixing `//export` and C definitions in the same file):

```go
var gHotkeyCh = make(chan struct{}, 1)

//export goHotkeyCallback
func goHotkeyCallback() {
    select {
    case gHotkeyCh <- struct{}{}:
    default:
    }
}
```

In `runListener()` select loop:

```go
case <-gHotkeyCh:
    if recording {
        l.StopRecording()
    } else {
        hudRecording()
        l.Record()  // starts capture regardless of meeting detection
    }
```

### Ad-hoc recording (no meeting detected)

- Calls `l.Record()` directly — same as meeting-triggered recording.
- Session folder named by timestamp: `2026-05-06-143022/` under the recordings root.
- If no meeting app is running, the system audio tap may yield silence; mic track is always captured.
- `MeetingUpdated` event (which carries app name / title) may never fire — that's fine. The folder is named by timestamp only.

---

## 3. Auto-Transcription

### What changes in main.go

| Before | After |
|--------|-------|
| `RecordingReady` → `overlayRecordingSaved()` → wait 120s for user tap | `RecordingReady` → `hudTranscribing()` → `runTranscription()` immediately |
| User chooses "Transcribe" or "Save for Later" | No choice — always transcribes |
| On transcription done → `cocoaNotify()` | On success → `hudHide()` |
| On transcription fail → `cocoaNotify()` | On fail → `hudError(msg)` |

`offerTranscription()` → renamed `runTranscription()`. The prompt-and-wait logic is removed.

### Removed overlay states

`overlayRecordingSaved()` and `waitOverlayPost()` are deleted from `overlay_darwin.go`. The corresponding ObjC HTML states (`recording-saved`, post-recording buttons) are removed from `overlayHTML` in `overlay_darwin.m`.

The `gOverlayPostCh` channel and its drain in `runListener` are removed.

---

## 4. Storage

Default output directory: `~/Documents/SideHuddle Recordings/`

Set via `listener.SetOutputDir()` at app startup. Both meeting-triggered and ad-hoc recordings land here.

Folder structure per session:
```
~/Documents/SideHuddle Recordings/
  2026-05-06-143022/          # ad-hoc (timestamp only)
    mic.wav
    mixed.wav
    others.wav
    transcript.txt            # if transcription succeeded
  2026-05-06-152210-Zoom-Weekly Sync/   # meeting (timestamp + app + title)
    mic.wav
    mixed.wav
    others.wav
    transcript.txt
```

---

## 5. Bug Fix — Stop Recording Menu Item

**Problem:** `goStopRecordingCallback()` in `notify_darwin.go` sends to `gStopRecordingCh` but nothing in `runListener` drains it. The menu-bar "Stop Recording" item is silently ignored.

**Fix:** Add to the `runListener` select loop:

```go
case <-gStopRecordingCh:
    l.StopRecording()
```

---

## 6. What Stays Unchanged

- Existing overlay (`overlay_darwin.m` / `overlay_darwin.go`) — keeps the "Record this meeting?" prompt and "Dismiss" flow. Only the post-recording states are removed.
- Rust core library — no changes.
- Go bindings in `bindings/go/` — no changes.
- CI / packaging — no changes.
- `pollMeetingTitle` goroutine — no changes.
- `organizeRecording()` — no changes (works for both meeting and ad-hoc since it takes the session folder path).

---

## 7. File Change Summary

| File | Change |
|------|--------|
| `cmd/side-huddle/hud_darwin.go` | **New** — Go HUD bridge |
| `cmd/side-huddle/hud_darwin.m` | **New** — ObjC NSPanel HUD |
| `cmd/side-huddle/hud_other.go` | **New** — non-darwin stub |
| `cmd/side-huddle/cocoa_darwin.m` | **Modified** — add CGEventTap, Accessibility check, "Grant Access" menu item |
| `cmd/side-huddle/cocoa_darwin.go` | **Modified** — add `cocoaIsAccessibilityTrusted()` wrapper |
| `cmd/side-huddle/main.go` | **Modified** — add hotkey select case, replace offerTranscription flow, add gStopRecordingCh drain, set default output dir |
| `cmd/side-huddle/hotkey_darwin.go` | **New** — `goHotkeyCallback` export + `gHotkeyCh` (separate file required by CGo) |
| `cmd/side-huddle/overlay_darwin.go` | **Modified** — remove `overlayRecordingSaved`, `waitOverlayPost`, `gOverlayPostCh` |
| `cmd/side-huddle/overlay_darwin.m` | **Modified** — remove post-recording HTML states from `overlayHTML` |
