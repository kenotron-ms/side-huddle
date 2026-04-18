"""
sidehuddle — Python bindings for the side-huddle Rust library.

Wraps the cdylib via ctypes.  No compilation step needed; just build the
Rust library first:

    cargo build --release          # from the repo root

Then use this package normally:

    from sidehuddle import Listener, EventKind
"""

import ctypes
import sys
from pathlib import Path


# ── Find the compiled library ─────────────────────────────────────────────────

def _find_lib() -> str:
    here = Path(__file__).resolve().parent
    root = here.parent.parent.parent          # repo root
    candidates = [
        root / "target" / "release" / "libside_huddle.dylib",   # macOS
        root / "target" / "release" / "libside_huddle.so",       # Linux
        root / "target" / "release" / "side_huddle.dll",         # Windows
    ]
    for p in candidates:
        if p.exists():
            return str(p)
    paths = "\n  ".join(str(p) for p in candidates)
    raise FileNotFoundError(
        f"libside_huddle not found.  Run: cargo build --release\n"
        f"Looked in:\n  {paths}"
    )

_lib = ctypes.CDLL(_find_lib())


# ── C struct (must match include/side_huddle.h exactly) ───────────────────────

class _SHEvent(ctypes.Structure):
    _fields_ = [
        ("kind",         ctypes.c_int),
        ("app",          ctypes.c_char_p),
        ("title",        ctypes.c_char_p),
        ("path",         ctypes.c_char_p),
        ("message",      ctypes.c_char_p),
        ("permission",   ctypes.c_int),
        ("perm_status",  ctypes.c_int),
        ("capture_kind", ctypes.c_int),
        ("capturing",    ctypes.c_int),
    ]

_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.POINTER(_SHEvent), ctypes.c_void_p)


# ── Function signatures ────────────────────────────────────────────────────────

_lib.side_huddle_new.restype          = ctypes.c_void_p
_lib.side_huddle_free.argtypes        = [ctypes.c_void_p]
_lib.side_huddle_on.argtypes          = [ctypes.c_void_p, _CB_TYPE, ctypes.c_void_p]
_lib.side_huddle_auto_record.argtypes = [ctypes.c_void_p]
_lib.side_huddle_record.argtypes      = [ctypes.c_void_p]
_lib.side_huddle_set_sample_rate.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
_lib.side_huddle_set_output_dir.argtypes  = [ctypes.c_void_p, ctypes.c_char_p]
_lib.side_huddle_start.restype        = ctypes.c_int
_lib.side_huddle_start.argtypes       = [ctypes.c_void_p]
_lib.side_huddle_stop.argtypes        = [ctypes.c_void_p]
_lib.side_huddle_version.restype      = ctypes.c_char_p


# ── Enum constants ─────────────────────────────────────────────────────────────

class EventKind:
    PERMISSION_STATUS   = 0
    PERMISSIONS_GRANTED = 1
    MEETING_DETECTED    = 2
    MEETING_UPDATED     = 3
    MEETING_ENDED       = 4
    RECORDING_STARTED   = 5
    RECORDING_ENDED     = 6
    RECORDING_READY     = 7
    CAPTURE_STATUS      = 8
    ERROR               = 9

    _names = {
        0: "PermissionStatus",   1: "PermissionsGranted",
        2: "MeetingDetected",    3: "MeetingUpdated",
        4: "MeetingEnded",       5: "RecordingStarted",
        6: "RecordingEnded",     7: "RecordingReady",
        8: "CaptureStatus",      9: "Error",
    }

    @classmethod
    def name(cls, v: int) -> str:
        return cls._names.get(v, f"EventKind({v})")

class Permission:
    MICROPHONE     = 0
    SCREEN_CAPTURE = 1
    ACCESSIBILITY  = 2

    _names = {0: "Microphone", 1: "ScreenCapture", 2: "Accessibility"}

    @classmethod
    def name(cls, v: int) -> str:
        return cls._names.get(v, f"Permission({v})")

class PermStatus:
    GRANTED       = 0
    NOT_REQUESTED = 1
    DENIED        = 2

    _names = {0: "Granted", 1: "NotRequested", 2: "Denied"}

    @classmethod
    def name(cls, v: int) -> str:
        return cls._names.get(v, f"PermStatus({v})")


# ── Python Event wrapper ───────────────────────────────────────────────────────

class Event:
    """A meeting lifecycle event delivered to registered handlers."""

    def __init__(self, c_ev: _SHEvent):
        self.kind         = c_ev.kind
        self.app          = (c_ev.app     or b"").decode()
        self.title        = (c_ev.title   or b"").decode()
        self.path         = (c_ev.path    or b"").decode()
        self.message      = (c_ev.message or b"").decode()
        self.permission   = c_ev.permission
        self.perm_status  = c_ev.perm_status
        self.capture_kind = c_ev.capture_kind
        self.capturing    = bool(c_ev.capturing)

    def __repr__(self):
        return (
            f"Event({EventKind.name(self.kind)}"
            + (f", app={self.app!r}" if self.app else "")
            + ")"
        )


# ── Listener ───────────────────────────────────────────────────────────────────

class Listener:
    """
    Detect Teams / Zoom / Google Meet sessions and emit lifecycle events.

    Usage::

        from sidehuddle import Listener, EventKind

        listener = Listener()

        @listener.on
        def _(event):
            print(event)

        with listener:
            input("Press Enter to stop...\n")
    """

    def __init__(self):
        self._handle    = _lib.side_huddle_new()
        self._callbacks = []   # keep ctypes callbacks alive (prevents GC)

    # ── Event registration ────────────────────────────────────────────────────

    def on(self, callback) -> "Listener":
        """Register an event handler.  Can be called multiple times."""
        def _bridge(c_ev_ptr, _userdata):
            try:
                callback(Event(c_ev_ptr.contents))
            except Exception as exc:
                print(f"side-huddle handler error: {exc}", file=sys.stderr)

        c_cb = _CB_TYPE(_bridge)
        self._callbacks.append(c_cb)      # prevent GC
        _lib.side_huddle_on(self._handle, c_cb, None)
        return self

    # ── Control ───────────────────────────────────────────────────────────────

    def auto_record(self) -> "Listener":
        """Record every detected meeting automatically."""
        _lib.side_huddle_auto_record(self._handle)
        return self

    def record(self) -> None:
        """Start recording the current meeting (call from MeetingDetected handler)."""
        _lib.side_huddle_record(self._handle)

    def set_sample_rate(self, hz: int) -> "Listener":
        """Set the PCM sample rate in Hz (default: 16000).  Call before start()."""
        _lib.side_huddle_set_sample_rate(self._handle, hz)
        return self

    def set_output_dir(self, path: str) -> "Listener":
        """Set the WAV output directory (default: cwd).  Call before start()."""
        _lib.side_huddle_set_output_dir(self._handle, path.encode())
        return self

    def start(self) -> None:
        """Start monitoring.  Events arrive via registered handlers."""
        if _lib.side_huddle_start(self._handle) != 0:
            raise RuntimeError("side-huddle: failed to start")

    def stop(self) -> None:
        """Stop monitoring and any active recording."""
        _lib.side_huddle_stop(self._handle)

    # ── Context manager ───────────────────────────────────────────────────────

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *_):
        self.stop()

    def __del__(self):
        if self._handle:
            _lib.side_huddle_free(self._handle)
            self._handle = None


# ── Module helpers ─────────────────────────────────────────────────────────────

def version() -> str:
    """Return the side-huddle library version string."""
    return _lib.side_huddle_version().decode()
