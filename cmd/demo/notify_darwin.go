//go:build darwin

package main

// notify_darwin.go — actionable macOS notifications for SideHuddle.
//
// Contains the //export goRecordChoiceCallback Go function (called from ObjC
// via _cgo_export.h) and thin Go wrappers for the two new ObjC entry points.
//
// CGo rule: a file with //export annotations may only contain DECLARATIONS
// (not definitions) in its C preamble.  All C function bodies live in
// cocoa_darwin.m; only forward prototypes are listed here.

/*
#include <stdlib.h>

// Forward declarations — bodies live in cocoa_darwin.m.
void sh_cocoa_notify_record_choice(const char *app);
void sh_cocoa_notify_with_folder(const char *title, const char *body,
                                  const char *folder);
*/
import "C"

import (
	"time"
	"unsafe"
)

// gRecordChoiceCh carries the user's recording decision back from the ObjC
// notification delegate into the Go event-handler goroutine.
// Capacity 1 so the delegate never blocks; stale values are drained before
// each new meeting-detected event.
var gRecordChoiceCh = make(chan bool, 1)

// goRecordChoiceCallback is called by the ObjC UNUserNotificationCenterDelegate
// when the user taps "Record" (shouldRecord=1), "Skip" (shouldRecord=0), or the
// notification body itself (treated as Record).  Exported so cocoa_darwin.m can
// call it directly after including _cgo_export.h.
//
//export goRecordChoiceCallback
func goRecordChoiceCallback(shouldRecord int32) {
	select {
	case gRecordChoiceCh <- shouldRecord != 0:
	default: // already a value pending; overwrite semantics not needed
	}
}

// cocoaNotifyRecordChoice posts an actionable "Meeting detected — record?"
// banner and returns a channel that receives the user's choice.  The caller
// should select on the channel with a timeout and treat expiry as "record".
func cocoaNotifyRecordChoice(app string) <-chan bool {
	// Drain any stale decision left over from a previous meeting.
	select {
	case <-gRecordChoiceCh:
	default:
	}
	ca := C.CString(app)
	defer C.free(unsafe.Pointer(ca))
	C.sh_cocoa_notify_record_choice(ca)
	return gRecordChoiceCh
}

// waitRecordChoice blocks until the user responds to the record-choice
// notification or timeout elapses (default: record).
func waitRecordChoice(ch <-chan bool, timeout time.Duration) bool {
	select {
	case record := <-ch:
		return record
	case <-time.After(timeout):
		return true // auto-record when no response
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
