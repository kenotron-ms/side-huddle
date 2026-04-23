//go:build darwin && cgo

package main

/*
#cgo LDFLAGS: -framework WebKit

#include <stdlib.h>

void overlay_warmup(void);
void overlay_set_state(const char *jsonCStr);
void overlay_hide_c(void);
void overlay_set_mouse(int ignore);
void overlay_open(const char *pathCStr);
*/
import "C"
import (
	"encoding/json"
	"fmt"
	"time"
	"unsafe"
)

// overlayState is serialised to JSON and passed to the WKWebView setState() call.
type overlayState struct {
	Title   string          `json:"title"`
	Sub     string          `json:"sub,omitempty"`
	Dot     string          `json:"dot"`
	Timer   bool            `json:"timer,omitempty"`
	Buttons []overlayButton `json:"buttons,omitempty"`
}

type overlayButton struct {
	Label   string `json:"label"`
	Action  string `json:"a"`
	Primary bool   `json:"p,omitempty"`
}

func shOverlayWarmup() { C.overlay_warmup() }

func shOverlayShow(s overlayState) {
	data, _ := json.Marshal(s)
	cs := C.CString(string(data))
	defer C.free(unsafe.Pointer(cs))
	C.overlay_set_state(cs)
}

func shOverlayHide() { C.overlay_hide_c() }

// ── Action routing ─────────────────────────────────────────────────────────

// gOverlayRecordCh receives the user's record/skip choice (true=record).
// Capacity 1 so the delegate never blocks; stale values are drained before each new meeting.
var gOverlayRecordCh = make(chan bool, 1)

// gOverlayPostCh receives the post-recording action string ("transcribe","later","dismiss").
var gOverlayPostCh = make(chan string, 1)

// shOverlayAction is exported to C and called by _SHOverlayDelegate when a
// button is tapped inside the WebView.
//
//export shOverlayAction
func shOverlayAction(actionCStr *C.char) {
	action := C.GoString(actionCStr)
	switch action {
	case "record":
		select {
		case gOverlayRecordCh <- true:
		default:
		}
	case "dismiss":
		// dismiss before recording = skip; dismiss after recording = close overlay
		select {
		case gOverlayRecordCh <- false:
		default:
		}
		select {
		case gOverlayPostCh <- "dismiss":
		default:
		}
		go shOverlayHide()
	case "transcribe", "later":
		select {
		case gOverlayPostCh <- action:
		default:
		}
		go shOverlayHide()
	case "open_settings":
		cs := C.CString("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
		C.overlay_open(cs)
		C.free(unsafe.Pointer(cs))
	}
}

// ── Overlay state helpers ──────────────────────────────────────────────────

func overlayMeetingDetected(app string) {
	// Drain stale record-choice from a prior meeting.
	select {
	case <-gOverlayRecordCh:
	default:
	}
	shOverlayShow(overlayState{
		Title: fmt.Sprintf("%s meeting detected", app),
		Sub:   "Record and transcribe?",
		Dot:   "green",
		Buttons: []overlayButton{
			{Label: "Record", Action: "record", Primary: true},
			{Label: "Dismiss", Action: "dismiss"},
		},
	})
}

func overlayRecording(app string) {
	shOverlayShow(overlayState{
		Title: fmt.Sprintf("Recording %s", app),
		Dot:   "red",
		Timer: true,
	})
}

func overlayRecordingSaved(durationSec int) {
	// Drain stale post-recording choice from a prior meeting.
	select {
	case <-gOverlayPostCh:
	default:
	}
	mins, secs := durationSec/60, durationSec%60
	shOverlayShow(overlayState{
		Title: "Recording saved",
		Sub:   fmt.Sprintf("%dm %ds · Transcribe with Whisper?", mins, secs),
		Dot:   "yellow",
		Buttons: []overlayButton{
			{Label: "Transcribe", Action: "transcribe", Primary: true},
			{Label: "Save for Later", Action: "later"},
		},
	})
}

func overlayTranscribing() {
	shOverlayShow(overlayState{
		Title: "Transcribing\u2026",
		Sub:   "This usually takes under a minute",
		Dot:   "blue",
	})
}

// waitOverlayRecord blocks until the user taps Record or Dismiss in the overlay,
// or 60 s elapses (returns false — skip recording).
func waitOverlayRecord() bool {
	select {
	case v := <-gOverlayRecordCh:
		return v
	case <-time.After(60 * time.Second):
		go shOverlayHide()
		return false
	}
}

// waitOverlayPost blocks until the user taps Transcribe, Save for Later, or Dismiss,
// or 120 s elapses (returns "later").
func waitOverlayPost() string {
	select {
	case v := <-gOverlayPostCh:
		return v
	case <-time.After(120 * time.Second):
		go shOverlayHide()
		return "later"
	}
}
