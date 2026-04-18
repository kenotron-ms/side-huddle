#!/usr/bin/env node
// side-huddle Node.js demo
//
// Build the native addon first:
//   cd crates/side-huddle-node && npx @napi-rs/cli build --platform --release
//
// Then run from the repo root:
//   node bindings/node/demo.js

// napi-rs/cli generates index.js — handles cross-platform .node loading
const { Listener, version } = require("../../crates/side-huddle-node");

console.log(`side-huddle ${version()} — waiting for Teams / Zoom / Google Meet…\n`);

const listener = new Listener();

// ── Handler 1: log every lifecycle event ─────────────────────────────────────
listener.on((event) => {
  const icons = {
    PermissionStatus:   event.status === "Granted" ? "✅" : event.status === "Denied" ? "❌" : "⏳",
    PermissionsGranted: "✅",
    MeetingDetected:    "🟢",
    MeetingUpdated:     "📋",
    RecordingStarted:   "⏺ ",
    MeetingEnded:       "🔴",
    RecordingEnded:     "⏹ ",
    RecordingReady:     "💾",
    CaptureStatus:      "📡",
    Error:              "⚠️ ",
  };
  const icon = icons[event.kind] ?? "  ";

  switch (event.kind) {
    case "PermissionStatus":
      console.log(`${icon}  permission: ${event.permission} → ${event.status}`);
      break;
    case "PermissionsGranted":
      console.log(`${icon}  all permissions granted`);
      break;
    case "MeetingDetected":
      console.log(`${icon}  detected:  ${event.app}`);
      break;
    case "MeetingUpdated":
      console.log(`${icon}  updated:   ${event.app} — "${event.title}"`);
      break;
    case "RecordingStarted":
      console.log(`${icon}  recording: ${event.app} started`);
      break;
    case "MeetingEnded":
      console.log(`${icon}  ended:     ${event.app}`);
      break;
    case "RecordingEnded":
      console.log(`${icon}  recording: ${event.app} stopped`);
      break;
    case "RecordingReady":
      console.log(`${icon}  saved:     ${event.app} → ${event.path}`);
      break;
    case "Error":
      console.error(`${icon}  error:     ${event.message}`);
      break;
  }
});

// ── Handler 2: auto-record (or prompt — swap as needed) ──────────────────────
listener.on((event) => {
  if (event.kind === "MeetingDetected") {
    console.log("   auto-recording…");
    listener.record();
  }
});

listener.start();
console.log("monitoring… (Ctrl-C to exit)");

process.on("SIGINT",  () => { listener.stop(); process.exit(0); });
process.on("SIGTERM", () => { listener.stop(); process.exit(0); });
