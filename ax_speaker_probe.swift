#!/usr/bin/env swift
/// ax_speaker_probe.swift — Quick test: can we see the active-speaker signal via AXObserver?
///
/// Usage:
///   swift ax_speaker_probe.swift                    # auto-detect meeting app
///   swift ax_speaker_probe.swift zoom               # search for "zoom" process
///   swift ax_speaker_probe.swift "Google Chrome"    # browser-based meetings
///   swift ax_speaker_probe.swift teams --dump       # dump full web-content tree to /tmp/ax_tree.txt
///
/// What this does:
///   1. Finds the PID of the target process
///   2. Walks the AX tree printing elements that look speaker-related
///   3. Watches ALL value/selected changes on the app element and prints them
///
/// --dump: instead of the filtered walk, dumps the COMPLETE web-content subtree
///         (unlimited depth) to /tmp/ax_tree.txt. Run this while INSIDE a meeting.
///
/// Requires Accessibility permission for the terminal running this.

import Cocoa
import ApplicationServices

// ── helpers ──────────────────────────────────────────────────────────────────

func axString(_ el: AXUIElement, _ attr: String) -> String? {
    var val: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el, attr as CFString, &val) == .success,
          let s = val as? String else { return nil }
    return s
}

func axChildren(_ el: AXUIElement) -> [AXUIElement] {
    var val: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el, kAXChildrenAttribute as CFString, &val) == .success,
          let arr = val as? [AXUIElement] else { return [] }
    return arr
}

func axRole(_ el: AXUIElement) -> String { axString(el, kAXRoleAttribute) ?? "?" }
func axTitle(_ el: AXUIElement) -> String? { axString(el, kAXTitleAttribute) }
func axDesc(_ el: AXUIElement) -> String? { axString(el, kAXDescriptionAttribute) }
func axValue(_ el: AXUIElement) -> String? { axString(el, kAXValueAttribute) }
func axHelp(_ el: AXUIElement) -> String? { axString(el, kAXHelpAttribute) }
func axLabel(_ el: AXUIElement) -> String? { axString(el, "AXLabel") }

/// Return all attribute names for an element (useful for exploration)
func axAllAttrs(_ el: AXUIElement) -> [String] {
    var names: CFArray?
    guard AXUIElementCopyAttributeNames(el, &names) == .success,
          let arr = names as? [String] else { return [] }
    return arr
}

// ── AX tree walker ───────────────────────────────────────────────────────────

let speakerKeywords = ["speak", "active", "mute", "participant", "unmute", "microphone", "audio"]

func looksLikeSpeaker(_ el: AXUIElement) -> Bool {
    let strings = [axTitle(el), axDesc(el), axValue(el), axHelp(el), axLabel(el)]
        .compactMap { $0 }.map { $0.lowercased() }
    return strings.contains { s in speakerKeywords.contains { s.contains($0) } }
}

func walk(_ el: AXUIElement, depth: Int = 0, maxDepth: Int = 8) {
    guard depth <= maxDepth else { return }
    let indent = String(repeating: "  ", count: depth)
    let role  = axRole(el)
    let title = axTitle(el) ?? ""
    let desc  = axDesc(el) ?? ""
    let val   = axValue(el) ?? ""

    if looksLikeSpeaker(el) || depth < 3 {
        let parts = [title, desc, val].filter { !$0.isEmpty }.joined(separator: " | ")
        print("\(indent)[\(role)] \(parts)")
        // Print extra attributes for interesting nodes
        if looksLikeSpeaker(el) {
            for attr in axAllAttrs(el) where !["AXRole","AXTitle","AXDescription","AXValue","AXChildren","AXParent","AXPosition","AXSize","AXFocused","AXEnabled","AXFrame"].contains(attr) {
                var v: CFTypeRef?
                if AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success {
                    print("\(indent)    ↳ \(attr) = \(v!)")
                }
            }
        }
    }

    for child in axChildren(el) {
        walk(child, depth: depth + 1, maxDepth: maxDepth)
    }
}

// ── Full dump (--dump mode) ───────────────────────────────────────────────────
// Writes the ENTIRE web-content AXGroup tree to /tmp/ax_tree.txt.
// Run this while you are INSIDE a meeting so participant tiles are in the tree.

func dumpNode(_ el: AXUIElement, depth: Int, into out: inout String) {
    let indent = String(repeating: "  ", count: depth)
    let role  = axRole(el)
    let title = axTitle(el) ?? ""
    let desc  = axDesc(el) ?? ""
    let val   = axValue(el) ?? ""
    let parts = [title, desc, val].filter { !$0.isEmpty }.joined(separator: " | ")
    out += "\(indent)[\(role)] \(parts)\n"

    // Print all non-geometric attributes for any node that looks interesting
    if looksLikeSpeaker(el) || depth < 4 {
        let skip = Set(["AXRole","AXTitle","AXDescription","AXValue","AXChildren",
                        "AXParent","AXPosition","AXSize","AXFocused","AXEnabled","AXFrame",
                        "AXVisibleChildren","AXRoleDescription"])
        for attr in axAllAttrs(el) where !skip.contains(attr) {
            var v: CFTypeRef?
            if AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success {
                out += "\(indent)  ↳ \(attr) = \(v!)\n"
            }
        }
    }

    for child in axChildren(el) {
        dumpNode(child, depth: depth + 1, into: &out)
    }
}

func runDump(appEl: AXUIElement) {
    print("Dumping full web-content tree (this may take a few seconds)...")
    var buf = ""
    // Find all AXGroups that are Chromium web content containers and dump them
    var dumped = 0
    for win in axChildren(appEl) {
        for child in axChildren(win) {
            let desc = axDesc(child) ?? axTitle(child) ?? ""
            if desc.contains("Web content") {
                buf += "=== \(desc) ===\n"
                dumpNode(child, depth: 0, into: &buf)
                buf += "\n"
                dumped += 1
            }
        }
    }
    if dumped == 0 {
        print("No 'Web content' group found. Are you inside a meeting window?")
        return
    }
    let path = "/tmp/ax_tree.txt"
    try? buf.write(toFile: path, atomically: true, encoding: .utf8)
    print("Wrote \(buf.count) bytes to \(path)")
    print("grep -i 'speak\\|mute\\|participant\\|active' \(path)")
}

// ── AXObserver callback ───────────────────────────────────────────────────────

let notifications: [String] = [
    kAXValueChangedNotification as String,
    kAXSelectedChildrenChangedNotification as String,
    kAXFocusedUIElementChangedNotification as String,
    kAXTitleChangedNotification as String,
    "AXLiveRegionChanged",
    "AXMenuItemSelected",
]

/// Dump the subtree of an AXMenuItem so we can see what child elements are inside.
func printMenuItemSubtree(_ el: AXUIElement, depth: Int = 0) {
    let indent = String(repeating: "  ", count: depth)
    let role  = axRole(el)
    let title = axTitle(el) ?? ""
    let desc  = axDesc(el) ?? ""
    let val   = axValue(el) ?? ""
    // Print ALL non-geometric attributes so nothing is hidden
    var attrs: [String] = []
    for attr in axAllAttrs(el) where !["AXChildren","AXParent","AXPosition","AXSize","AXFrame","AXVisibleChildren"].contains(attr) {
        var v: CFTypeRef?
        if AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success {
            let s = "\(v!)"
            if !s.isEmpty && s != "(null)" { attrs.append("\(attr)=\(s)") }
        }
    }
    let summary = attrs.joined(separator: "  ")
    print("\(indent)[\(role)] \(summary)")
    for child in axChildren(el) {
        printMenuItemSubtree(child, depth: depth + 1)
    }
}

/// Walk tree finding all AXMenuItem elements, attach per-element observers for title/value changes.
/// Also recurses into each AXMenuItem's children to catch sub-element changes.
func watchMenuItems(appEl: AXUIElement, obs: AXObserver) {
    // Every notification type — silently ignore unsupported ones
    let allNotifs: [String] = [
        kAXValueChangedNotification as String,
        kAXTitleChangedNotification as String,
        kAXSelectedChildrenChangedNotification as String,
        kAXUIElementDestroyedNotification as String,
        kAXCreatedNotification as String,
        kAXFocusedUIElementChangedNotification as String,
        "AXLiveRegionChanged",
        "AXElementBusyChanged",
        "AXCheckedChanged",
        "AXExpandedChanged",
        "AXSelectedTextChanged",
    ]

    func attachAll(_ el: AXUIElement) {
        for notif in allNotifs {
            AXObserverAddNotification(obs, el, notif as CFString, nil)
        }
        for child in axChildren(el) { attachAll(child) }
    }

    func recurse(_ el: AXUIElement, depth: Int) {
        guard depth < 20 else { return }
        if axRole(el) == "AXMenuItem" {
            let title = axTitle(el) ?? axDesc(el) ?? "(no title)"
            print("  📌 AXMenuItem: \"\(title)\"")
            print("     └─ subtree:")
            printMenuItemSubtree(el, depth: 4)
            print()
            // Attach observers to the menuitem AND all its descendants
            attachAll(el)
        }
        for child in axChildren(el) {
            recurse(child, depth: depth + 1)
        }
    }
    recurse(appEl, depth: 0)
}

func startObserver(pid: pid_t, focusMenuItems: Bool = false) -> AXObserver? {
    var observer: AXObserver?
    let err = AXObserverCreate(pid, { (obs, el, notif, userData) in
        let notifName = notif as String
        let role  = axString(el, kAXRoleAttribute) ?? "?"
        let title = axString(el, kAXTitleAttribute) ?? ""
        let desc  = axString(el, kAXDescriptionAttribute) ?? ""
        let val   = axString(el, kAXValueAttribute) ?? ""
        let parts = [title, desc, val].filter { !$0.isEmpty }.joined(separator: " | ")
        let ts = DateFormatter.localizedString(from: Date(), dateStyle: .none, timeStyle: .medium)
        // Highlight speaker-related changes
        let isSpeaker = parts.lowercased().contains("speak") || parts.lowercased().contains("unmut")
        let prefix = isSpeaker ? "🎤" : "  "
        print("\(prefix) [\(ts)] \(notifName)  [\(role)] \(parts)")
    }, &observer)

    guard err == .success, let obs = observer else {
        print("AXObserverCreate failed: \(err.rawValue)")
        return nil
    }

    let appEl = AXUIElementCreateApplication(pid)

    if focusMenuItems {
        // Attach per-element observers to all AXMenuItems (participant tiles)
        print("Scanning for AXMenuItem participant tiles...")
        watchMenuItems(appEl: appEl, obs: obs)
        // Also watch live regions on the whole app
        AXObserverAddNotification(obs, appEl, "AXLiveRegionChanged" as CFString, nil)
        print("  ✓ watching AXLiveRegionChanged on app")
    } else {
        for notif in notifications {
            let r = AXObserverAddNotification(obs, appEl, notif as CFString, nil)
            if r == .success { print("  ✓ watching \(notif)") }
        }
    }

    CFRunLoopAddSource(CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(obs), .defaultMode)
    return obs
}

// ── Poll/diff mode (--poll) ───────────────────────────────────────────────────
// Snapshots every AXMenuItem subtree every N ms and prints anything that changed.
// Catches attribute changes that never emit a notification.

typealias Snapshot = [String: String]   // "depth:role:index → attr=value"

func snapshotElement(_ el: AXUIElement, prefix: String) -> Snapshot {
    var snap = Snapshot()
    let role = axRole(el)
    let skip = Set(["AXChildren","AXParent","AXPosition","AXSize","AXFrame",
                    "AXVisibleChildren","AXTopLevelUIElement","AXWindow"])
    for attr in axAllAttrs(el) where !skip.contains(attr) {
        var v: CFTypeRef?
        if AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success {
            snap["\(prefix).\(attr)"] = "\(v!)"
        }
    }
    let children = axChildren(el)
    for (i, child) in children.enumerated() {
        let childSnap = snapshotElement(child, prefix: "\(prefix)/\(role)[\(i)]")
        snap.merge(childSnap) { _, new in new }
    }
    return snap
}

func snapshotAllMenuItems(appEl: AXUIElement) -> Snapshot {
    var result = Snapshot()
    func recurse(_ el: AXUIElement, depth: Int) {
        guard depth < 20 else { return }
        if axRole(el) == "AXMenuItem" {
            let title = axTitle(el) ?? axDesc(el) ?? "?"
            let snap = snapshotElement(el, prefix: title)
            result.merge(snap) { _, new in new }
        }
        for child in axChildren(el) { recurse(child, depth: depth + 1) }
    }
    recurse(appEl, depth: 0)
    return result
}

func runPoll(appEl: AXUIElement, intervalMs: Int) {
    print("Polling AXMenuItem subtrees every \(intervalMs)ms — mute/unmute or speak to see changes...")
    var prev = snapshotAllMenuItems(appEl: appEl)
    print("Initial snapshot: \(prev.count) attributes across all participant tiles.")
    print()

    Timer.scheduledTimer(withTimeInterval: Double(intervalMs) / 1000.0, repeats: true) { _ in
        let curr = snapshotAllMenuItems(appEl: appEl)
        let ts = DateFormatter.localizedString(from: Date(), dateStyle: .none, timeStyle: .medium)

        // Added or changed
        for (key, val) in curr {
            if let old = prev[key] {
                if old != val {
                    print("[\(ts)] CHANGED  \(key)")
                    print("         old: \(old)")
                    print("         new: \(val)")
                }
            } else {
                print("[\(ts)] ADDED    \(key) = \(val)")
            }
        }
        // Removed
        for key in prev.keys where curr[key] == nil {
            print("[\(ts)] REMOVED  \(key)")
        }

        prev = curr
    }
    RunLoop.current.run()
}

// ── main ──────────────────────────────────────────────────────────────────────

let args = CommandLine.arguments.dropFirst()
let dumpMode         = args.contains("--dump")
let participantMode  = args.contains("--participants")
let pollMode         = args.contains("--poll")          // snapshot+diff every 200ms
let searchName = args.filter { !$0.hasPrefix("--") }.first?.lowercased()

let meetingApps = ["zoom", "microsoft teams", "google chrome", "microsoft edge", "slack"]
let apps = NSWorkspace.shared.runningApplications

let targets = apps.filter { app in
    guard let name = app.localizedName?.lowercased() else { return false }
    if let s = searchName { return name.contains(s) }
    return meetingApps.contains(where: { name.contains($0) })
}

guard let target = targets.first else {
    let names = apps.compactMap { $0.localizedName }.joined(separator: ", ")
    print("No meeting app found. Running apps: \(names)")
    print("Try: swift ax_speaker_probe.swift \"App Name\"")
    exit(1)
}

let pid = target.processIdentifier
print("Target: \(target.localizedName ?? "?") (PID \(pid))")
print()

// Check accessibility permission
if !AXIsProcessTrusted() {
    print("⚠️  Accessibility permission NOT granted for this terminal.")
    print("   System Settings → Privacy & Security → Accessibility → add Terminal/iTerm")
    print("   Then re-run.")
    exit(1)
}

let appEl = AXUIElementCreateApplication(pid)

if dumpMode {
    runDump(appEl: appEl)
    exit(0)
}

if pollMode {
    runPoll(appEl: appEl, intervalMs: 200)
    // runLoop inside runPoll, never returns
}

// Walk the AX tree looking for speaker-related elements
print("=== AX Tree (speaker-related nodes + top 3 levels) ===")
walk(appEl, maxDepth: 8)

print()
print("=== Watching for AX notifications (Ctrl-C to stop) ===")
let obs = startObserver(pid: pid, focusMenuItems: participantMode)
guard obs != nil else { exit(1) }

RunLoop.current.run()
