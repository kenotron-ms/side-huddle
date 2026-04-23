//go:build darwin

    package main

    // notify_darwin.go — actionable macOS notifications for SideHuddle.
    //
    // Contains //export Go functions (called from ObjC via _cgo_export.h) and thin
    // Go wrappers for the ObjC notification / alert entry points.
    //
    // CGo rule: a file with //export annotations may only contain DECLARATIONS
    // (not definitions) in its C preamble.  All C function bodies live in
    // cocoa_darwin.m; only forward prototypes are listed here.

    /*
    #include <stdlib.h>
    #include <stdint.h>

    // Forward declarations — bodies live in cocoa_darwin.m.
    void sh_cocoa_notify_record_choice(const char *app);
    void sh_cocoa_notify_with_folder(const char *title, const char *body,
                                      const char *folder);
    void sh_cocoa_show_record_alert(const char *app);
    */
    import "C"

    import (
    	"time"
    	"unsafe"
    )

    // gRecordChoiceCh carries the user's recording decision back from the ObjC
    // notification delegate or NSAlert into the Go event-handler goroutine.
    // Capacity 1 so the delegate never blocks; stale values are drained before
    // each new meeting-detected event.
    var gRecordChoiceCh = make(chan bool, 1)

    // gStopRecordingCh is closed/sent by the ObjC "Stop Recording" menu item.
    // The main select loop drains it and calls listener.StopRecording().
    var gStopRecordingCh = make(chan struct{}, 1)

    // goRecordChoiceCallback is called by the ObjC UNUserNotificationCenterDelegate
    // when the user taps "Record" (shouldRecord=1), "Skip" (shouldRecord=0), or the
    // notification body itself (treated as Record).  Also called by the NSAlert
    // fallback when the user clicks a button.
    // Exported so cocoa_darwin.m can call it directly after including _cgo_export.h.
    //
    //export goRecordChoiceCallback
    func goRecordChoiceCallback(shouldRecord int32) {
    	select {
    	case gRecordChoiceCh <- shouldRecord != 0:
    	default: // already a value pending; first response wins
    	}
    }

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

    // cocoaNotifyRecordChoice posts an actionable "Meeting detected — record?"
    // banner and returns a channel that receives the user's choice.
    // NOTE: this requires notification permission.  Prefer cocoaAlertRecordChoice
    // for reliable prompting that works without any system permission.
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

    // cocoaAlertRecordChoice shows a modal NSAlert asking whether to record the
    // current meeting and returns a channel that receives the user's choice.
    // Unlike cocoaNotifyRecordChoice this works without notification permission
    // and produces a clearly visible on-screen dialog.
    func cocoaAlertRecordChoice(app string) <-chan bool {
    	// Drain any stale decision left over from a previous meeting.
    	select {
    	case <-gRecordChoiceCh:
    	default:
    	}
    	ca := C.CString(app)
    	defer C.free(unsafe.Pointer(ca))
    	C.sh_cocoa_show_record_alert(ca)
    	return gRecordChoiceCh
    }

    // waitRecordChoice blocks until the user responds to the record-choice
    // prompt or timeout elapses. Returns false on timeout so that recording
    // never starts without explicit user confirmation.
    func waitRecordChoice(ch <-chan bool, timeout time.Duration) bool {
    	select {
    	case record := <-ch:
    		return record
    	case <-time.After(timeout):
    		return false // no response — skip recording
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
    