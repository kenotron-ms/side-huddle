//go:build darwin

package main

/*
#cgo CFLAGS: -fobjc-arc
#cgo LDFLAGS: -framework AppKit -framework Foundation -framework UserNotifications -framework ServiceManagement -framework AVFoundation -framework CoreGraphics -framework ScreenCaptureKit
#include <stdlib.h>
void sh_cocoa_activate(void);
void sh_cocoa_run(void);
void sh_cocoa_terminate(void);
void sh_cocoa_notify(const char *title, const char *body);
void sh_cocoa_set_recording(int recording, const char *app, const char *title);
const char *sh_cocoa_find_meeting_title(const char *app);
*/
import "C"

import (
	"runtime"
	"unsafe"
)

func init() {
	// Pin the Go main goroutine to the OS main thread — Cocoa's NSApp run
	// loop must execute there, and any AppKit call from a different thread
	// is undefined behavior.
	runtime.LockOSThread()
}

// cocoaActivate initializes NSApplication with a Regular activation policy
// and brings the app to the foreground. Must be called on the main thread.
func cocoaActivate() { C.sh_cocoa_activate() }

// cocoaRun blocks on [NSApp run] until cocoaTerminate is called. Pumps the
// Cocoa event loop so permission dialogs render, gain focus, and the Dock
// icon stops bouncing.
func cocoaRun() { C.sh_cocoa_run() }

// cocoaTerminate asks NSApp to exit its run loop. Safe to call from any
// goroutine — it hops to the main queue internally.
func cocoaTerminate() { C.sh_cocoa_terminate() }

// cocoaNotify posts a local macOS notification (banner + default sound).
// Safe to call from any goroutine. First call triggers a one-time TCC
// authorization prompt; subsequent calls are silent.
func cocoaNotify(title, body string) {
	ct := C.CString(title)
	cb := C.CString(body)
	defer C.free(unsafe.Pointer(ct))
	defer C.free(unsafe.Pointer(cb))
	C.sh_cocoa_notify(ct, cb)
}

// cocoaFindMeetingTitle scans on-screen windows for a non-chrome window owned
// by `app` and returns its title. Returns "" if none is found. The Rust core's
// window watcher is lazy and emits MeetingUpdated only once at detection, so
// we poll this periodically to pick up the meeting window once the user has
// it in the foreground.
func cocoaFindMeetingTitle(app string) string {
	ca := C.CString(app)
	defer C.free(unsafe.Pointer(ca))
	ct := C.sh_cocoa_find_meeting_title(ca)
	if ct == nil {
		return ""
	}
	defer C.free(unsafe.Pointer(ct))
	return C.GoString(ct)
}

// cocoaSetRecording flips the menu-bar icon + title to reflect an in-progress
// meeting recording. Pass recording=false to return to the idle waveform icon.
// Safe from any goroutine — the native side hops to the main queue internally.
func cocoaSetRecording(recording bool, app, title string) {
	var r C.int
	if recording {
		r = 1
	}
	ca := C.CString(app)
	ct := C.CString(title)
	defer C.free(unsafe.Pointer(ca))
	defer C.free(unsafe.Pointer(ct))
	C.sh_cocoa_set_recording(r, ca, ct)
}
