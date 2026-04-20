//go:build darwin

package main

/*
#cgo CFLAGS: -fobjc-arc
#cgo LDFLAGS: -framework AppKit -framework Foundation -framework UserNotifications
#include <stdlib.h>
void sh_cocoa_activate(void);
void sh_cocoa_run(void);
void sh_cocoa_terminate(void);
void sh_cocoa_notify(const char *title, const char *body);
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
