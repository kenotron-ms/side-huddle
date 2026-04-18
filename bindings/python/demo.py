#!/usr/bin/env python3
"""
side-huddle Python demo

Usage:
    python bindings/python/demo.py
"""
import sys
import signal
import threading

sys.path.insert(0, __file__.rsplit("/bindings", 1)[0] + "/bindings/python")
from sidehuddle import Listener, EventKind, Permission, PermStatus, version

print(f"side-huddle {version()} — waiting for Teams / Zoom / Google Meet...\n")

done = threading.Event()
listener = Listener()

# ── Handler 1: log every event ────────────────────────────────────────────────
@listener.on
def _(event):
    k = event.kind
    if k == EventKind.PERMISSION_STATUS:
        icons = {PermStatus.GRANTED: "✅", PermStatus.NOT_REQUESTED: "⏳", PermStatus.DENIED: "❌"}
        print(f"{icons.get(event.perm_status,'?')}  permission: "
              f"{Permission.name(event.permission)} → {PermStatus.name(event.perm_status)}")
    elif k == EventKind.PERMISSIONS_GRANTED:
        print("✅  all permissions granted")
    elif k == EventKind.MEETING_DETECTED:
        print(f"🟢  detected:  {event.app}")
    elif k == EventKind.MEETING_UPDATED:
        print(f"📋  updated:   {event.app} — {event.title!r}")
    elif k == EventKind.RECORDING_STARTED:
        print(f"⏺   recording: {event.app} started")
    elif k == EventKind.MEETING_ENDED:
        print(f"🔴  ended:     {event.app}")
    elif k == EventKind.RECORDING_ENDED:
        print(f"⏹   recording: {event.app} stopped")
    elif k == EventKind.RECORDING_READY:
        print(f"💾  saved:     {event.app} → {event.path}")
    elif k == EventKind.ERROR:
        print(f"⚠️   error:     {event.message}", file=sys.stderr)

# ── Handler 2: prompt user to record ─────────────────────────────────────────
@listener.on
def _(event):
    if event.kind == EventKind.MEETING_DETECTED:
        answer = input(f"   Record {event.app}? [y/N] ")
        if answer.strip().lower() == "y":
            listener.record()

# ── Start ─────────────────────────────────────────────────────────────────────
listener.start()

def _shutdown(sig, frame):
    print("\nshutting down…")
    listener.stop()
    sys.exit(0)

signal.signal(signal.SIGINT,  _shutdown)
signal.signal(signal.SIGTERM, _shutdown)

print("monitoring… (Ctrl-C to exit)")
signal.pause()   # block until signal
