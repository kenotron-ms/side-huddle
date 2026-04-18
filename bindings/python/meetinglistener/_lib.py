"""
meetinglistener._lib — raw ctypes bindings to libmeetinglistener.

Loads the shared library and declares all C function signatures.
Do not use this module directly — import MeetingListener from meetinglistener.
"""

import ctypes
import os
import pathlib
import platform

# ── Library location ──────────────────────────────────────────────────────────

_LIB_NAMES = {
    "darwin":  "libmeetinglistener.dylib",
    "windows": "meetinglistener.dll",
    "linux":   "libmeetinglistener.so",
}


def _find_lib() -> str:
    """Locate libmeetinglistener on disk.

    Search order:
    1. MEETINGLISTENER_LIB env var (explicit override)
    2. Package directory (installed wheel — dylib bundled next to __init__.py)
    3. core/build/ walking up from this file (development build)
    """
    if env := os.environ.get("MEETINGLISTENER_LIB"):
        return env

    sys_name = platform.system().lower()
    lib_name = _LIB_NAMES.get(sys_name, "libmeetinglistener.so")

    # Installed: dylib sits next to _lib.py
    pkg_dir = pathlib.Path(__file__).parent
    if (pkg_dir / lib_name).exists():
        return str(pkg_dir / lib_name)

    # Development: walk up to repo root and find core/build/<lib>
    root = pathlib.Path(__file__).resolve()
    for _ in range(8):
        root = root.parent
        candidate = root / "core" / "build" / lib_name
        if candidate.exists():
            return str(candidate)

    raise OSError(
        f"Cannot find {lib_name}.\n"
        "Build with:  cd core && cmake -B build && cmake --build build\n"
        "Or set:      MEETINGLISTENER_LIB=/path/to/libmeetinglistener.dylib"
    )


def _load() -> ctypes.CDLL:
    lib = ctypes.CDLL(_find_lib())

    # ── Callback function types (keep in sync with meetinglistener.h) ─────────

    lib.MeetingStartFn = ctypes.CFUNCTYPE(
        None,
        ctypes.c_char_p,   # app
        ctypes.c_int,      # pid
        ctypes.c_void_p,   # ctx
    )
    lib.MeetingEndFn = ctypes.CFUNCTYPE(
        None,
        ctypes.c_char_p,   # app
        ctypes.c_void_p,   # ctx
    )
    lib.ErrorFn = ctypes.CFUNCTYPE(
        None,
        ctypes.c_char_p,   # message
        ctypes.c_void_p,   # ctx
    )
    lib.AudioChunkFn = ctypes.CFUNCTYPE(
        None,
        ctypes.POINTER(ctypes.c_int16),  # pcm
        ctypes.c_int,                     # n_frames
        ctypes.c_int,                     # sample_rate
        ctypes.c_void_p,                  # ctx
    )

    # ── Function signatures ────────────────────────────────────────────────────

    lib.ml_new.restype  = ctypes.c_void_p
    lib.ml_new.argtypes = []

    lib.ml_free.restype  = None
    lib.ml_free.argtypes = [ctypes.c_void_p]

    lib.ml_start.restype  = ctypes.c_int
    lib.ml_start.argtypes = [
        ctypes.c_void_p,    # handle
        lib.MeetingStartFn,
        lib.MeetingEndFn,
        lib.ErrorFn,
        ctypes.c_void_p,    # ctx
    ]

    lib.ml_stop.restype  = None
    lib.ml_stop.argtypes = [ctypes.c_void_p]

    lib.ml_tap_start.restype  = ctypes.c_int
    lib.ml_tap_start.argtypes = [
        ctypes.c_void_p, ctypes.c_int, ctypes.c_int,
        lib.AudioChunkFn, ctypes.c_void_p,
    ]
    lib.ml_tap_stop.restype  = None
    lib.ml_tap_stop.argtypes = [ctypes.c_void_p]

    lib.ml_mic_start.restype  = ctypes.c_int
    lib.ml_mic_start.argtypes = [
        ctypes.c_void_p, ctypes.c_int, ctypes.c_int,
        lib.AudioChunkFn, ctypes.c_void_p,
    ]
    lib.ml_mic_stop.restype  = None
    lib.ml_mic_stop.argtypes = [ctypes.c_void_p]

    lib.ml_mix.restype  = None
    lib.ml_mix.argtypes = [
        ctypes.POINTER(ctypes.c_int16),
        ctypes.POINTER(ctypes.c_int16),
        ctypes.POINTER(ctypes.c_int16),
        ctypes.c_int,
    ]

    lib.ml_version.restype  = ctypes.c_char_p
    lib.ml_version.argtypes = []

    return lib


# Module-level singleton — loaded once on first import
_lib: ctypes.CDLL | None = None


def get_lib() -> ctypes.CDLL:
    global _lib
    if _lib is None:
        _lib = _load()
    return _lib
