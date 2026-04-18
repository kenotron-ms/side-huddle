#!/usr/bin/env swift
/// screen_speaker_probe.swift — detect active speaker via Teams window pixel analysis
///
/// Captures the Teams meeting window directly (no occlusion issues) using
/// SCContentFilter with the Teams app — renders through any overlapping windows.
/// Then samples the border pixels of each participant tile (from AX tree) to
/// detect the colored speaking ring.
///
/// Usage:
///   swift screen_speaker_probe.swift              # single snapshot
///   swift screen_speaker_probe.swift --save       # also save /tmp/teams_frame.png
///   swift screen_speaker_probe.swift --watch      # poll every 100ms

import Cocoa
import ApplicationServices
import ScreenCaptureKit
import UniformTypeIdentifiers

// ── AX helpers ────────────────────────────────────────────────────────────────

func axString(_ el: AXUIElement, _ attr: String) -> String? {
    var v: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success,
          let s = v as? String else { return nil }
    return s
}

func axChildren(_ el: AXUIElement) -> [AXUIElement] {
    var v: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el, kAXChildrenAttribute as CFString, &v) == .success,
          let arr = v as? [AXUIElement] else { return [] }
    return arr
}

func axPoint(_ el: AXUIElement, _ attr: String) -> CGPoint? {
    var v: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success,
          let val = v else { return nil }
    var pt = CGPoint.zero
    guard AXValueGetValue(val as! AXValue, .cgPoint, &pt) else { return nil }
    return pt
}

func axSize(_ el: AXUIElement, _ attr: String) -> CGSize? {
    var v: CFTypeRef?
    guard AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success,
          let val = v else { return nil }
    var sz = CGSize.zero
    guard AXValueGetValue(val as! AXValue, .cgSize, &sz) else { return nil }
    return sz
}

struct Tile {
    let name: String
    let frame: CGRect   // screen coordinates (pts), y=0 at top
}

func findTiles(pid: pid_t) -> [Tile] {
    var tiles: [Tile] = []
    func recurse(_ el: AXUIElement, depth: Int) {
        guard depth < 25 else { return }
        if axString(el, kAXRoleAttribute) == "AXMenuItem" {
            let name = axString(el, kAXTitleAttribute)
                    ?? axString(el, kAXDescriptionAttribute)
                    ?? "(unnamed)"
            if let pos  = axPoint(el, kAXPositionAttribute),
               let size = axSize(el, kAXSizeAttribute),
               size.width > 10 && size.height > 10 {
                tiles.append(Tile(name: name, frame: CGRect(origin: pos, size: size)))
            }
        }
        for child in axChildren(el) { recurse(child, depth: depth + 1) }
    }
    recurse(AXUIElementCreateApplication(pid), depth: 0)
    return tiles
}

// ── Window capture ────────────────────────────────────────────────────────────

struct CaptureResult {
    let image: CGImage
    let winFrame: CGRect    // screen coords of the meeting window
    let displayFrame: CGRect
}

func captureMeetingWindow(mainPid: pid_t) async -> CaptureResult? {
    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        guard let display = content.displays.first else { print("No display"); return nil }

        // Find the meeting window — has Teams in title but not Calendar
        let meetingWin = content.windows
            .filter { $0.owningApplication?.applicationName.contains("Teams") == true }
            .filter { w in
                let t = w.title ?? ""
                return t.contains("Microsoft Teams") && !t.contains("Calendar") && w.frame.width > 800
            }
            .max(by: { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height })

        guard let win = meetingWin, let meetingApp = win.owningApplication else {
            print("No meeting window found — are you in a call?"); return nil
        }

        // Filter by Teams app — captures all Teams windows through any occlusion
        let apps = content.applications.filter { $0.processID == meetingApp.processID }
        let filter = SCContentFilter(display: display, including: apps, exceptingWindows: [])
        let cfg = SCStreamConfiguration()
        cfg.width  = Int(display.frame.width)
        cfg.height = Int(display.frame.height)
        cfg.capturesAudio = false

        let fullImg = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: cfg)

        // Crop to the meeting window
        // SCWindow.frame and display.frame share the same coordinate origin (top-left)
        // CGImage.cropping uses bottom-left origin — flip y
        let sx = CGFloat(fullImg.width)  / display.frame.width
        let sy = CGFloat(fullImg.height) / display.frame.height
        let relX = (win.frame.minX - display.frame.minX) * sx
        let relY = (win.frame.minY - display.frame.minY) * sy
        let cgY  = CGFloat(fullImg.height) - relY - win.frame.height * sy
        let crop = CGRect(x: relX, y: cgY, width: win.frame.width * sx, height: win.frame.height * sy)

        guard let cropped = fullImg.cropping(to: crop) else {
            print("Crop failed"); return nil
        }
        return CaptureResult(image: cropped, winFrame: win.frame, displayFrame: display.frame)
    } catch {
        print("Capture error: \(error)"); return nil
    }
}

// ── Pixel analysis ────────────────────────────────────────────────────────────

struct RGBA { var r, g, b, a: UInt8 }

func pixelsOf(_ img: CGImage) -> (UnsafeMutablePointer<RGBA>, Int, Int) {
    let w = img.width, h = img.height
    let buf = UnsafeMutablePointer<RGBA>.allocate(capacity: w * h)
    buf.initialize(repeating: RGBA(r: 0, g: 0, b: 0, a: 0), count: w * h)
    let ctx = CGContext(data: buf, width: w, height: h,
                        bitsPerComponent: 8, bytesPerRow: w * 4,
                        space: CGColorSpaceCreateDeviceRGB(),
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
    return (buf, w, h)
}

func isSaturated(_ p: RGBA, threshold: Int = 35) -> Bool {
    let r = Int(p.r), g = Int(p.g), b = Int(p.b)
    let maxC = max(r, max(g, b)), minC = min(r, min(g, b))
    return (maxC - minC) > threshold && maxC > 40
}

func sampleBorder(pixels: UnsafeMutablePointer<RGBA>, imgW: Int, imgH: Int,
                  rect: CGRect, bw: Int = 4) -> (ratio: Double, hex: String) {
    let x0 = max(0, Int(rect.minX)), y0 = max(0, Int(rect.minY))
    let x1 = min(imgW - 1, Int(rect.maxX)), y1 = min(imgH - 1, Int(rect.maxY))
    guard x1 > x0 && y1 > y0 else { return (0, "#000000") }

    var rS = 0, gS = 0, bS = 0, chroma = 0, n = 0
    func s(_ x: Int, _ y: Int) {
        guard x >= 0 && x < imgW && y >= 0 && y < imgH else { return }
        let p = pixels[y * imgW + x]
        rS += Int(p.r); gS += Int(p.g); bS += Int(p.b)
        if isSaturated(p) { chroma += 1 }
        n += 1
    }
    for d in 0..<bw {
        for x in stride(from: x0, through: x1, by: 2) { s(x, y0+d); s(x, y1-d) }
        for y in stride(from: y0, through: y1, by: 2) { s(x0+d, y); s(x1-d, y) }
    }
    let hex = n > 0 ? String(format: "#%02X%02X%02X", rS/n, gS/n, bS/n) : "#000000"
    return (n > 0 ? Double(chroma)/Double(n) : 0, hex)
}

// ── Snapshot ──────────────────────────────────────────────────────────────────

func snapshot(pid: pid_t, saveMode: Bool) async -> String? {
    guard let cap = await captureMeetingWindow(mainPid: pid) else { return nil }
    let img = cap.image

    if saveMode {
        let url = URL(fileURLWithPath: "/tmp/teams_frame.png")
        if let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) {
            CGImageDestinationAddImage(dest, img, nil)
            CGImageDestinationFinalize(dest)
            print("Saved /tmp/teams_frame.png (\(img.width)×\(img.height)px)")
        }
    }

    // img coords: top-left = meeting window top-left
    let scaleX = CGFloat(img.width)  / cap.winFrame.width
    let scaleY = CGFloat(img.height) / cap.winFrame.height

    let (pixels, imgW, imgH) = pixelsOf(img)
    defer { pixels.deallocate() }

    let tiles = findTiles(pid: pid)
    guard !tiles.isEmpty else { print("No AXMenuItem tiles — are you in a meeting?"); return nil }

    var speaker: String? = nil
    let ts = DateFormatter.localizedString(from: Date(), dateStyle: .none, timeStyle: .medium)
    print("[\(ts)] \(tiles.count) tiles  \(imgW)×\(imgH)px")

    for tile in tiles {
        // Screen → window-relative → image pixels
        let rx = (tile.frame.minX - cap.winFrame.minX) * scaleX
        let ry = (tile.frame.minY - cap.winFrame.minY) * scaleY
        let tileRect = CGRect(x: rx, y: ry,
                              width:  tile.frame.width  * scaleX,
                              height: tile.frame.height * scaleY)

        let (ratio, hex) = sampleBorder(pixels: pixels, imgW: imgW, imgH: imgH, rect: tileRect)
        let speaking = ratio > 0.15
        print("  \(speaking ? "🎤" : "  ") \(tile.name.prefix(50))")
        print("       \(hex)  chromatic=\(String(format: "%.0f%%", ratio*100))")
        if speaking { speaker = tile.name }
    }
    return speaker
}

// ── Main ──────────────────────────────────────────────────────────────────────

let args        = CommandLine.arguments.dropFirst()
let watchMode   = args.contains("--watch")
let saveMode    = args.contains("--save")
let searchName  = args.filter { !$0.hasPrefix("--") }.first?.lowercased() ?? "microsoft teams"

guard let target = NSWorkspace.shared.runningApplications
    .first(where: { ($0.localizedName ?? "").lowercased().contains(searchName) })
else { print("App not found: \(searchName)"); exit(1) }

let pid = target.processIdentifier
print("Target: \(target.localizedName ?? "?") (PID \(pid))")
guard AXIsProcessTrusted() else { print("⚠️  Need Accessibility permission"); exit(1) }

let sema = DispatchSemaphore(value: 0)
Task {
    if watchMode {
        print("Polling at 100ms (Ctrl-C to stop)...\n")
        var last: String? = "UNINIT"
        while true {
            let cur = await snapshot(pid: pid, saveMode: false)
            if cur != last {
                let ts = DateFormatter.localizedString(from: Date(), dateStyle: .none, timeStyle: .medium)
                print(cur != nil ? "🎤 [\(ts)] SPEAKING: \(cur!)\n" : "   [\(ts)] silence\n")
                last = cur
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
    } else {
        _ = await snapshot(pid: pid, saveMode: saveMode)
        sema.signal()
    }
}
if watchMode { RunLoop.main.run() } else { sema.wait() }
