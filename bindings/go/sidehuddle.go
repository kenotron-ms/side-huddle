// Package sidehuddle provides Go bindings for the side-huddle Rust library.
//
// On darwin/arm64 a pre-built static archive is embedded under
// bindings/go/lib/ — no Rust toolchain required.
//
// On linux, windows, and darwin/amd64 the cdylib must be built first:
//
//	cd crates/side-huddle && cargo build --release
//
// Then build/run via the repo Makefile:
//
//	make run-demo
package sidehuddle

    /*
    #cgo CFLAGS: -I${SRCDIR}/../../include
    #cgo darwin,arm64 LDFLAGS: ${SRCDIR}/lib/darwin_arm64/libside_huddle.a -framework CoreAudio -framework CoreGraphics -framework CoreFoundation -framework AVFoundation
    #cgo darwin,amd64 LDFLAGS: -L${SRCDIR}/../../target/release -lside_huddle -Wl,-rpath,${SRCDIR}/../../target/release -framework CoreAudio -framework CoreGraphics -framework CoreFoundation -framework AVFoundation
    #cgo linux   LDFLAGS: -L${SRCDIR}/../../target/release -lside_huddle -Wl,-rpath,${SRCDIR}/../../target/release
    #cgo windows LDFLAGS: -L${SRCDIR}/../../target/release -lside_huddle

    #include "side_huddle.h"
    #include <stdlib.h>

    // Forward-declare the Go bridge (defined with //export in bridge.go).
    // CGo requires this to be a declaration only (no body) in a file that
    // also contains definitions.
    extern void goEventBridge(SHEvent* event, void* userdata);

    // C shim — same signature as SHEventCallback.
    static void goEventBridgeShim(const SHEvent* event, void* userdata) {
        goEventBridge((SHEvent*)event, userdata);
    }

    // C helper that registers the shim as the callback.
    // Using a helper avoids CGo's inability to take the address of a C static
    // function in Go code.
    static void registerGoCallback(SideHuddleHandle h, void* userdata) {
        side_huddle_on(h, goEventBridgeShim, userdata);
    }
    */
    import "C"

    import (
    	"fmt"
    	"sync"
    	"sync/atomic"
    	"unsafe"
    )

    // ── Event types ───────────────────────────────────────────────────────────────

    // EventKind identifies a meeting lifecycle event.
    type EventKind int

    const (
    	PermissionStatus   EventKind = 0
    	PermissionsGranted EventKind = 1
    	MeetingDetected    EventKind = 2
    	MeetingUpdated     EventKind = 3
    	MeetingEnded       EventKind = 4
    	RecordingStarted   EventKind = 5
    	RecordingEnded     EventKind = 6
    	RecordingReady     EventKind = 7
    	CaptureStatus      EventKind = 8
    	Error              EventKind = 9
    )

    // Permission identifies a system permission.
    type Permission int

    const (
    	Microphone    Permission = 0
    	ScreenCapture Permission = 1
    	Accessibility Permission = 2
    )

    // PermStatus is the grant state of a permission.
    type PermStatus int

    const (
    	Granted      PermStatus = 0
    	NotRequested PermStatus = 1
    	Denied       PermStatus = 2
    )

    // CaptureKind identifies the type of media stream in a CaptureStatus event.
    type CaptureKind int

    const (
    	Audio CaptureKind = 0
    	Video CaptureKind = 1
    )

    // Event holds data for a meeting lifecycle event.
    // Check Kind first, then read the relevant fields.
    type Event struct {
    	Kind EventKind

    	App     string // MeetingDetected/Updated/Ended, Recording*
    	Title   string // MeetingUpdated
    	Path    string // RecordingReady — path to the WAV file
    	Message string // Error

    	Permission  Permission
    	PermStatus  PermStatus
    	CaptureKind CaptureKind
    	Capturing   bool
    }

    // ── Callback registry ─────────────────────────────────────────────────────────
    // cgo cannot pass Go closures to C directly.  We keep them in a map, pass the
    // integer ID as the C userdata pointer, and look up on the way back.

    var (
    	cbMu      sync.RWMutex
    	callbacks = map[uintptr]func(*Event){}
    	nextID    atomic.Uint64
    )

    func registerCallback(f func(*Event)) uintptr {
    	id := uintptr(nextID.Add(1))
    	cbMu.Lock()
    	callbacks[id] = f
    	cbMu.Unlock()
    	return id
    }

    func unregisterAll(ids []uintptr) {
    	cbMu.Lock()
    	for _, id := range ids {
    		delete(callbacks, id)
    	}
    	cbMu.Unlock()
    }

    // cEventToGo converts a C SHEvent to a Go Event.
    func cEventToGo(ev *C.SHEvent) *Event {
    	e := &Event{Kind: EventKind(ev.kind)}
    	if ev.app     != nil { e.App     = C.GoString(ev.app) }
    	if ev.title   != nil { e.Title   = C.GoString(ev.title) }
    	if ev.path    != nil { e.Path    = C.GoString(ev.path) }
    	if ev.message != nil { e.Message = C.GoString(ev.message) }
    	e.Permission  = Permission(ev.permission)
    	e.PermStatus  = PermStatus(ev.perm_status)
    	e.CaptureKind = CaptureKind(ev.capture_kind)
    	e.Capturing   = ev.capturing != 0
    	return e
    }

    
    // ── String methods for readable output ───────────────────────────────────────

    func (p Permission) String() string {
    	switch p {
    	case Microphone:    return "Microphone"
    	case ScreenCapture: return "ScreenCapture"
    	case Accessibility: return "Accessibility"
    	default:            return fmt.Sprintf("Permission(%d)", int(p))
    	}
    }

    func (s PermStatus) String() string {
    	switch s {
    	case Granted:      return "Granted"
    	case NotRequested: return "NotRequested"
    	case Denied:       return "Denied"
    	default:           return fmt.Sprintf("PermStatus(%d)", int(s))
    	}
    }

    func (k EventKind) String() string {
    	names := []string{
    		"PermissionStatus", "PermissionsGranted",
    		"MeetingDetected", "MeetingUpdated", "MeetingEnded",
    		"RecordingStarted", "RecordingEnded", "RecordingReady",
    		"CaptureStatus", "Error",
    	}
    	if int(k) >= 0 && int(k) < len(names) {
    		return names[k]
    	}
    	return fmt.Sprintf("EventKind(%d)", int(k))
    }
    
// ── Listener ──────────────────────────────────────────────────────────────────

    // Listener detects meetings and emits lifecycle events via registered handlers.
    // Always call Stop() when done.
    type Listener struct {
    	handle C.SideHuddleHandle
    	ids    []uintptr
    	mu     sync.Mutex
    }

    // New creates a Listener with default settings (16 kHz, cwd output directory).
    func New() *Listener {
    	return &Listener{handle: C.side_huddle_new()}
    }

    // On registers an event handler.  Multiple handlers are called in order.
    func (l *Listener) On(f func(*Event)) *Listener {
    	id := registerCallback(f)
    	l.mu.Lock()
    	l.ids = append(l.ids, id)
    	l.mu.Unlock()
    	C.registerGoCallback(l.handle, unsafe.Pointer(id))
    	return l
    }

    // AutoRecord makes the listener record every detected meeting automatically.
    func (l *Listener) AutoRecord() *Listener {
    	C.side_huddle_auto_record(l.handle)
    	return l
    }

    // Record starts recording the current meeting.
    // Call from within a MeetingDetected handler to opt in.
    func (l *Listener) Record() {
    	C.side_huddle_record(l.handle)
    }

    // SetSampleRate sets the PCM sample rate in Hz (default: 16000).
    // Must be called before Start.
    func (l *Listener) SetSampleRate(hz uint32) *Listener {
    	C.side_huddle_set_sample_rate(l.handle, C.uint32_t(hz))
    	return l
    }

    // SetOutputDir sets the directory where WAV files are written (default: cwd).
    // Must be called before Start.
    func (l *Listener) SetOutputDir(dir string) *Listener {
    	cs := C.CString(dir)
    	defer C.free(unsafe.Pointer(cs))
    	C.side_huddle_set_output_dir(l.handle, cs)
    	return l
    }

    // Start begins monitoring. Returns an error if the library fails to initialise.
    func (l *Listener) Start() error {
    	if C.side_huddle_start(l.handle) != 0 {
    		return fmt.Errorf("side-huddle: start failed")
    	}
    	return nil
    }

    // Stop halts monitoring, cancels any active recording, and releases resources.
    func (l *Listener) Stop() {
    	C.side_huddle_stop(l.handle)
    	l.mu.Lock()
    	ids := l.ids
    	l.ids = nil
    	l.mu.Unlock()
    	unregisterAll(ids)
    	C.side_huddle_free(l.handle)
    	l.handle = nil
    }

    // Version returns the side-huddle library version string.
    func Version() string {
    	return C.GoString(C.side_huddle_version())
    }
    