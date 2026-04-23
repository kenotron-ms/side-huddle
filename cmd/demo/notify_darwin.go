//go:build darwin

package main

// notify_darwin.go — macOS notification helpers for SideHuddle.
//
// Contains //export Go functions (called from ObjC via _cgo_export.h) and thin
// Go wrappers for the ObjC notification entry points.
//
// CGo rule: a file with //export annotations may only contain DECLARATIONS
// (not definitions) in its C preamble.  All C function bodies live in
// cocoa_darwin.m; only forward prototypes are listed here.

/*
#include <stdlib.h>

// Forward declarations — bodies live in cocoa_darwin.m.
void sh_cocoa_notify_with_folder(const char *title, const char *body,
                                  const char *folder);
*/
import "C"

import "unsafe"

// gStopRecordingCh is closed/sent by the ObjC "Stop Recording" menu item.
// The main select loop drains it and calls listener.StopRecording().
var gStopRecordingCh = make(chan struct{}, 1)

// goStopRecordingCallback is called by the ObjC "Stop Recording" menu item.
// Exported so cocoa_darwin.m can call it via _cgo_export.h.
//
//export goStopRecordingCallback
func goStopRecordingCallback() {
	select {
	case gStopRecordingCh <- struct{}{}:
	default:
	}
}

// cocoaNotifyWithFolder posts a notification with an "Open Folder" action
// button.  Tapping the button opens folderPath in Finder.
func cocoaNotifyWithFolder(title, body, folderPath string) {
	ct := C.CString(title)
	cb := C.CString(body)
	cf := C.CString(folderPath)
	defer C.free(unsafe.Pointer(ct))
	defer C.free(unsafe.Pointer(cb))
	defer C.free(unsafe.Pointer(cf))
	C.sh_cocoa_notify_with_folder(ct, cb, cf)
}
