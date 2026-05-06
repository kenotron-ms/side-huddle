# Recording HUD + ⌥Space Hotkey Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the full-card post-recording overlay with a persistent floating HUD pill, add a global ⌥Space hotkey for ad-hoc recording, auto-transcribe on `RecordingReady`, and fix the no-op Stop Recording menu item.

**Architecture:** A new `NSPanel` HUD (200×40pt pill, top-right) is driven by `setState(json)` via WKWebView — the same CGo bridge pattern as the existing overlay. A CGEventTap installed in `sh_cocoa_activate()` converts ⌥Space keydowns into Go channel sends; the main `runListener` goroutine handles them in a new `for+select` loop. Post-recording overlay states are deleted; `offerTranscription` is renamed to `runTranscription` and invoked immediately on `RecordingReady` with no user prompt.

**Tech Stack:** Go 1.22+, CGo, Objective-C (ARC), AppKit, WebKit, ApplicationServices, macOS 13+.

---

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `cmd/side-huddle/hotkey_darwin.go` | **New** | `//export goHotkeyCallback` + `gHotkeyCh` channel. Separate file required by CGo: files with `//export` may only have declarations (no bodies) in their C preamble. |
| `cmd/side-huddle/hud_darwin.m` | **New** | ObjC `NSPanel` HUD — panel/WKWebView creation, full HTML/CSS/JS, C API (`hud_warmup`, `hud_set_state`, `hud_hide_c`), `_SHHudDelegate` script handler. |
| `cmd/side-huddle/hud_darwin.go` | **New** | Go HUD bridge — marshals state structs to JSON, calls C API; `//export shHudAction` for ObjC dismiss callback. |
| `cmd/side-huddle/hud_other.go` | **New** | No-op stubs for all HUD functions + `gHotkeyCh` stub variable for non-darwin builds. |
| `cmd/side-huddle/cocoa_darwin.m` | **Modified** | Add `#import <ApplicationServices/…>`, `shHotkeyCallback` CGEventTap function, CGEventTap installation + Accessibility check in `sh_cocoa_activate()`, `openAccessibilitySettings:` method on `SHController`. |
| `cmd/side-huddle/cocoa_darwin.go` | **Modified** | Add `-framework ApplicationServices` to `#cgo LDFLAGS`. |
| `cmd/side-huddle/main.go` | **Modified** | (1) `hudWarmup()` at startup; (2) `hudRecording()` replaces `overlayRecording()` in `MeetingDetected`; (3) auto-transcription in `RecordingReady`; (4) `for+select` loop with `gStopRecordingCh` drain + hotkey toggle. |
| `cmd/side-huddle/overlay_darwin.go` | **Modified** | Remove `gOverlayPostCh`, `overlayRecordingSaved`, `waitOverlayPost`, `overlayTranscribing`, `overlayRecording`; simplify `shOverlayAction` to drop `"transcribe"`/`"later"` cases. |
| `cmd/side-huddle/overlay_darwin.m` | **Modified** | Remove dead CSS: `.red{…animation:p…}`, `.yellow{…}`, `.blue{…}`, `@keyframes p{…}` (all used only by removed Go functions). |

---

## Task 1: `hotkey_darwin.go` — exported ⌥Space callback

**Files:**
- Create: `cmd/side-huddle/hotkey_darwin.go`

- [ ] **Step 1: Create the file**

CGo rule: a Go file with `//export` annotations may only have forward *declarations* (not definitions) in its C preamble. The actual CGEventTap callback (`shHotkeyCallback`) that calls `goHotkeyCallback` lives in `cocoa_darwin.m`. This file just exports the Go side of the channel handoff.

```go
// cmd/side-huddle/hotkey_darwin.go
//go:build darwin

package main

/*
#include <stdlib.h>
// No C definitions here.  CGo rule: a file with //export annotations may
// only contain forward declarations in its C preamble — definitions belong
// in a .m/.c file.  The CGEventTap callback in cocoa_darwin.m calls
// goHotkeyCallback() via the _cgo_export.h header that CGo auto-generates.
*/
import "C"

// gHotkeyCh receives a token each time the ⌥Space hotkey fires.
// Capacity 1 — rapid presses before the loop drains are coalesced into one toggle.
var gHotkeyCh = make(chan struct{}, 1)

// goHotkeyCallback is called by the CGEventTap installed in sh_cocoa_activate()
// (cocoa_darwin.m) whenever ⌥Space is detected.
//
//export goHotkeyCallback
func goHotkeyCallback() {
	select {
	case gHotkeyCh <- struct{}{}:
	default:
	}
}
```

- [ ] **Step 2: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0.

- [ ] **Step 3: Commit**

```bash
git add cmd/side-huddle/hotkey_darwin.go
git commit -m "feat(hotkey): add goHotkeyCallback export and gHotkeyCh channel"
```

---

## Task 2: CGEventTap wiring — `cocoa_darwin.m` + `cocoa_darwin.go`

**Files:**
- Modify: `cmd/side-huddle/cocoa_darwin.go` (LDFLAGS line)
- Modify: `cmd/side-huddle/cocoa_darwin.m` (import, callback fn, SHController method, sh_cocoa_activate)

- [ ] **Step 1: Add `-framework ApplicationServices` to `cocoa_darwin.go` LDFLAGS**

Replace the existing `#cgo LDFLAGS` line:

```go
// OLD (line 7 in cocoa_darwin.go):
#cgo LDFLAGS: -framework AppKit -framework Foundation -framework UserNotifications -framework ServiceManagement -framework AVFoundation -framework CoreGraphics -framework ScreenCaptureKit

// NEW:
#cgo LDFLAGS: -framework AppKit -framework Foundation -framework UserNotifications -framework ServiceManagement -framework AVFoundation -framework CoreGraphics -framework ScreenCaptureKit -framework ApplicationServices
```

- [ ] **Step 2: Add `#import <ApplicationServices/ApplicationServices.h>` to `cocoa_darwin.m`**

The file currently starts with `#import <AppKit/AppKit.h>`. Add the new import on the line after the existing CoreGraphics import:

```objc
// After the existing imports at the top of cocoa_darwin.m, add:
#import <ApplicationServices/ApplicationServices.h>
```

The import block should now read:
```objc
#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ApplicationServices/ApplicationServices.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <UserNotifications/UserNotifications.h>
#import <ServiceManagement/ServiceManagement.h>
#include "_cgo_export.h"  // goRecordChoiceCallback
```

- [ ] **Step 3: Add `shHotkeyCallback` CGEventTap function to `cocoa_darwin.m`**

Add this static function directly before the `// ── Menu controller ──` section comment (before the `SHController @interface` line):

```objc
// ── ⌥Space hotkey (CGEventTap) ──────────────────────────────────────────────

static CGEventRef shHotkeyCallback(CGEventTapProxy proxy, CGEventType type,
                                    CGEventRef event, void *refcon) {
    (void)proxy; (void)refcon;
    if (type == kCGEventKeyDown) {
        CGKeyCode keyCode = (CGKeyCode)CGEventGetIntegerValueField(
            event, kCGKeyboardEventKeycode);
        CGEventFlags flags = CGEventGetFlags(event);
        // Key 49 = Space; require Option only (no Cmd, Ctrl, or Shift).
        if (keyCode == 49 &&
            (flags & kCGEventFlagMaskAlternate) &&
            !(flags & (kCGEventFlagMaskCommand |
                       kCGEventFlagMaskControl  |
                       kCGEventFlagMaskShift))) {
            goHotkeyCallback();
            return NULL; // swallow the event
        }
    }
    return event;
}
```

- [ ] **Step 4: Add `openAccessibilitySettings:` method to `SHController` in `cocoa_darwin.m`**

Add this method to `@implementation SHController`, after the existing `openScreenRecordingSettings:` method:

```objc
- (void)openAccessibilitySettings:(id)sender {
    // Prompt the user to grant Accessibility TCC permission so CGEventTap
    // can intercept ⌥Space globally.
    NSDictionary *opts = @{(__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES};
    AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)opts);
}
```

- [ ] **Step 5: Install CGEventTap and Accessibility menu item in `sh_cocoa_activate()`**

In the `if (@available(macOS 13.0, *))` block of `sh_cocoa_activate()`, after the `[menu addItem:[NSMenuItem separatorItem]]` line that precedes the `stopItem` block — and before the line `c.headerItem = header;` — add the Accessibility menu item:

```objc
        // ── Accessibility permission check for ⌥Space hotkey ─────────────
        if (!AXIsProcessTrusted()) {
            [menu addItem:[NSMenuItem separatorItem]];
            NSMenuItem *axItem = [menu addItemWithTitle:@"Grant Accessibility Access\u2026"
                                                  action:@selector(openAccessibilitySettings:)
                                           keyEquivalent:@""];
            axItem.target = c;
        }
```

Then, at the very end of `sh_cocoa_activate()`, after `gStatusItem.menu = menu;` and before the closing `}`, add:

```objc
    // ── Install ⌥Space global hotkey via CGEventTap ──────────────────────
    // Requires Accessibility TCC permission (com.apple.security.accessibility).
    // On first launch without the grant, the tap creation returns NULL and
    // the fallback NSLog reminds the user to grant access.
    CGEventMask mask = CGEventMaskBit(kCGEventKeyDown);
    CFMachPortRef tap = CGEventTapCreate(
        kCGSessionEventTap,
        kCGHeadInsertEventTap,
        kCGEventTapOptionDefault,
        mask,
        shHotkeyCallback,
        NULL);
    if (tap) {
        CFRunLoopSourceRef src = CFMachPortCreateRunLoopSource(NULL, tap, 0);
        CFRunLoopAddSource(CFRunLoopGetMain(), src, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        CFRelease(src);
        CFRelease(tap);
    } else {
        NSLog(@"SideHuddle: CGEventTap failed — grant Accessibility access "
               "in System Settings → Privacy & Security → Accessibility");
    }
```

- [ ] **Step 6: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0.

- [ ] **Step 7: Commit**

```bash
git add cmd/side-huddle/cocoa_darwin.go cmd/side-huddle/cocoa_darwin.m
git commit -m "feat(hotkey): install CGEventTap for ⌥Space in sh_cocoa_activate"
```

---

## Task 3: `hud_darwin.m` — ObjC NSPanel HUD

**Files:**
- Create: `cmd/side-huddle/hud_darwin.m`

The HUD is a 220×52pt borderless `NSPanel` at `NSFloatingWindowLevel`, positioned 8pt from the top-right corner of the visible screen area. A WKWebView fills it with a 200×40pt pill that slides in/out via CSS transitions. State is driven by `setState(json)` the same way as the existing overlay.

- [ ] **Step 1: Create `cmd/side-huddle/hud_darwin.m`**

```objc
// HUD pill — persistent floating status indicator.
// NSPanel + WKWebView; independent of overlay_darwin.m.
// Driven by setState(json) via evaluateJavaScript, same pattern as the overlay.

#import <Cocoa/Cocoa.h>
#import <WebKit/WebKit.h>

// shHudAction is defined in hud_darwin.go (via //export) and declared here
// so this file can call it from the WKScriptMessageHandler delegate.
extern void shHudAction(const char *action);

// ── Delegate ──────────────────────────────────────────────────────────────────

@interface _SHHudDelegate : NSObject <WKScriptMessageHandler>
@end

@implementation _SHHudDelegate
- (void)userContentController:(WKUserContentController *)ucc
      didReceiveScriptMessage:(WKScriptMessage *)msg {
    NSString *action = (NSString *)msg.body ?: @"";
    shHudAction(action.UTF8String);
}
@end

// ── Module state ──────────────────────────────────────────────────────────────

static NSPanel        *_gHud     = nil;
static WKWebView      *_gHudView = nil;
static _SHHudDelegate *_gHudDel  = nil;

// ── HTML ──────────────────────────────────────────────────────────────────────
//
// Pill spec:
//   background: rgba(28,28,30,0.92), backdrop-filter:blur(20px), border-radius:22px
//   size: ~200×40pt, positioned top-right 8pt from panel edge
//   recording:    green  #30d158, 3 animated waveform bars, elapsed MM:SS timer
//   transcribing: blue   #0a84ff, spinning ring 14px/0.75s, "Transcribing" label
//   error:        red    #ff453a, ✕ glyph, "Transcription failed" + message, clickable

static NSString *hudHTML =
    @"<!DOCTYPE html>"
    "<html><head><meta charset='utf-8'><style>"
    "*{margin:0;padding:0;box-sizing:border-box}"
    "html,body{background:transparent;width:100%;height:100%;overflow:hidden;"
    "font-family:-apple-system,BlinkMacSystemFont,'SF Pro Text',sans-serif;"
    "-webkit-font-smoothing:antialiased}"
    "#pill{position:absolute;top:6px;right:8px;"
    "display:flex;align-items:center;gap:8px;"
    "padding:0 14px 0 10px;height:40px;min-width:140px;"
    "background:rgba(28,28,30,0.92);"
    "backdrop-filter:blur(20px) saturate(180%);"
    "-webkit-backdrop-filter:blur(20px) saturate(180%);"
    "border-radius:22px;"
    "border:0.5px solid rgba(255,255,255,0.08);"
    "transform:translateX(calc(100% + 30px));opacity:0;"
    "transition:transform 0.3s cubic-bezier(0.34,1.56,0.64,1),opacity 0.25s ease}"
    "#pill.on{transform:translateX(0);opacity:1}"
    "#icon{width:28px;height:28px;border-radius:50%;"
    "border:1.5px solid rgba(255,255,255,0.2);"
    "display:flex;align-items:center;justify-content:center;flex-shrink:0}"
    ".bars{display:flex;align-items:center;gap:2px}"
    ".bar{width:3px;border-radius:2px;animation:bp 0.9s ease-in-out infinite}"
    ".bar:nth-child(1){height:10px;animation-delay:0s}"
    ".bar:nth-child(2){height:16px;animation-delay:0.15s}"
    ".bar:nth-child(3){height:10px;animation-delay:0.3s}"
    "@keyframes bp{0%,100%{transform:scaleY(1);opacity:1}"
    "50%{transform:scaleY(0.4);opacity:0.6}}"
    ".ring{width:14px;height:14px;border-radius:50%;"
    "border:2px solid rgba(255,255,255,0.18);border-top-color:#0a84ff;"
    "animation:sp 0.75s linear infinite}"
    "@keyframes sp{to{transform:rotate(360deg)}}"
    "#right{display:flex;align-items:center;flex:1;min-width:0}"
    ".l1{font-size:12px;font-weight:600;line-height:1.2;white-space:nowrap}"
    ".l2{font-size:10px;margin-top:2px;color:rgba(255,255,255,0.4);"
    "line-height:1.2;white-space:nowrap}"
    "#timer{font-size:12px;font-weight:500;font-variant-numeric:tabular-nums;"
    "color:rgba(255,255,255,0.55);white-space:nowrap;margin-left:4px}"
    "</style></head><body>"
    "<div id='pill'>"
    "<div id='icon'></div>"
    "<div id='right'></div>"
    "</div>"
    "<script>"
    "var iv=null;"
    "function s(a){window.webkit.messageHandlers.sh.postMessage(a)}"
    "function fmt(n){var m=Math.floor(n/60),s=n%60;return m+':'+(s<10?'0':'')+s}"
    "function setState(d){"
    "clearInterval(iv);iv=null;"
    "var pill=document.getElementById('pill');"
    "if(!d){pill.classList.remove('on');return}"
    "var icon=document.getElementById('icon');"
    "var right=document.getElementById('right');"
    "icon.innerHTML='';icon.style.cssText='';right.innerHTML='';"
    "pill.style.border='';pill.onclick=null;pill.style.cursor='default';"
    "if(d.type==='recording'){"
    "pill.style.border='0.5px solid rgba(48,209,88,0.22)';"
    "icon.style.background='rgba(48,209,88,0.15)';"
    "icon.style.borderColor='#30d158';"
    "icon.innerHTML=\"<div class='bars'>"
    "<div class='bar' style='background:#30d158'></div>"
    "<div class='bar' style='background:#30d158'></div>"
    "<div class='bar' style='background:#30d158'></div>"
    "</div>\";"
    "var sec=d.elapsed||0;"
    "var tm=document.createElement('span');"
    "tm.id='timer';tm.textContent=fmt(sec);right.appendChild(tm);"
    "iv=setInterval(function(){"
    "sec++;document.getElementById('timer').textContent=fmt(sec)"
    "},1000);"
    "}else if(d.type==='transcribing'){"
    "pill.style.border='0.5px solid rgba(10,132,255,0.22)';"
    "icon.style.background='rgba(10,132,255,0.15)';"
    "icon.style.borderColor='#0a84ff';"
    "icon.innerHTML=\"<div class='ring'></div>\";"
    "var lbl=document.createElement('span');"
    "lbl.className='l1';lbl.style.color='rgba(255,255,255,0.7)';"
    "lbl.textContent='Transcribing';right.appendChild(lbl);"
    "}else if(d.type==='error'){"
    "pill.style.border='0.5px solid rgba(255,69,58,0.35)';"
    "pill.style.cursor='pointer';"
    "icon.style.background='rgba(255,69,58,0.15)';"
    "icon.style.borderColor='#ff453a';"
    "icon.innerHTML=\"<span style='color:#ff453a;font-size:13px;"
    "font-weight:600'>\\u2715</span>\";"
    "var wrap=document.createElement('div');"
    "wrap.style.display='flex';wrap.style.flexDirection='column';"
    "var l1=document.createElement('div');"
    "l1.className='l1';l1.style.color='#ff453a';"
    "l1.textContent='Transcription failed';"
    "var l2=document.createElement('div');"
    "l2.className='l2';l2.textContent=d.message||'';"
    "wrap.appendChild(l1);wrap.appendChild(l2);"
    "right.appendChild(wrap);"
    "pill.onclick=function(){s('dismiss')}"
    "}"
    "requestAnimationFrame(function(){pill.classList.add('on')})"
    "}"
    "</script></body></html>";

// ── C API ─────────────────────────────────────────────────────────────────────

static void hud_ensure_created(void) {
    if (_gHud) return;

    NSRect screen = [NSScreen mainScreen].visibleFrame;
    // Panel is slightly wider/taller than the pill to give the slide-in
    // animation room without clipping.  The pill itself is inset 6pt top,
    // 8pt right inside the WebView.
    CGFloat w = 220, h = 52;
    NSRect frame = NSMakeRect(
        NSMaxX(screen) - w - 8,
        NSMaxY(screen) - h - 8,
        w, h);

    _gHud = [[NSPanel alloc]
        initWithContentRect:frame
        styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
        backing:NSBackingStoreBuffered
        defer:NO];

    _gHud.level             = NSFloatingWindowLevel;
    _gHud.opaque            = NO;
    _gHud.backgroundColor   = [NSColor clearColor];
    _gHud.hasShadow         = NO;
    _gHud.collectionBehavior =
        NSWindowCollectionBehaviorCanJoinAllSpaces |
        NSWindowCollectionBehaviorStationary       |
        NSWindowCollectionBehaviorIgnoresCycle;
    [_gHud setAnimationBehavior:NSWindowAnimationBehaviorNone];

    WKWebViewConfiguration *cfg = [[WKWebViewConfiguration alloc] init];
    [cfg.userContentController addScriptMessageHandler:
        (_gHudDel = [[_SHHudDelegate alloc] init]) name:@"sh"];

    _gHudView = [[WKWebView alloc]
        initWithFrame:NSMakeRect(0, 0, w, h)
        configuration:cfg];
    [_gHudView setValue:@NO forKey:@"drawsBackground"];
    _gHud.contentView = _gHudView;
    [_gHudView loadHTMLString:hudHTML baseURL:nil];
}

// hud_set_state drives the pill's visual state.  jsonCStr must be one of:
//   {"type":"recording","elapsed":0}
//   {"type":"transcribing"}
//   {"type":"error","message":"<short message>"}
//   (null literal)  — hides the pill
void hud_set_state(const char *jsonCStr) {
    NSString *json = [NSString stringWithUTF8String:jsonCStr];
    dispatch_async(dispatch_get_main_queue(), ^{
        hud_ensure_created();
        NSString *js = [NSString stringWithFormat:@"setState(%@)", json];
        [_gHudView evaluateJavaScript:js completionHandler:nil];
        [_gHud orderFrontRegardless];
    });
}

// hud_warmup pre-creates the panel at launch to eliminate first-show lag.
void hud_warmup(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        hud_ensure_created();
    });
}

// hud_hide_c removes the pill: JS transition runs for ~300 ms, then the
// panel is ordered out so it stops consuming compositing resources.
void hud_hide_c(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!_gHud) return;
        [_gHudView evaluateJavaScript:@"setState(null)" completionHandler:nil];
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 350 * NSEC_PER_MSEC),
            dispatch_get_main_queue(), ^{
                [_gHud orderOut:nil];
            });
    });
}
```

- [ ] **Step 2: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0. (The `.m` file compiles as part of CGo even though no Go file imports it yet — it will be imported in Task 4.)

- [ ] **Step 3: Commit**

```bash
git add cmd/side-huddle/hud_darwin.m
git commit -m "feat(hud): add ObjC NSPanel HUD implementation"
```

---

## Task 4: `hud_darwin.go` + `hud_other.go` — Go HUD bridge

**Files:**
- Create: `cmd/side-huddle/hud_darwin.go`
- Create: `cmd/side-huddle/hud_other.go`

- [ ] **Step 1: Create `cmd/side-huddle/hud_darwin.go`**

The `//export shHudAction` directive is safe here because the C preamble contains only forward declarations (no function bodies). The actual C implementations are in `hud_darwin.m`.

```go
// cmd/side-huddle/hud_darwin.go
//go:build darwin && cgo

package main

/*
#cgo LDFLAGS: -framework WebKit

#include <stdlib.h>

// Forward declarations — bodies are in hud_darwin.m.
void hud_warmup(void);
void hud_set_state(const char *jsonCStr);
void hud_hide_c(void);
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"unsafe"
)

// hudWarmup pre-creates the HUD panel at launch so the first show is instant.
func hudWarmup() { C.hud_warmup() }

// hudRecording shows the green recording state with an elapsed-seconds counter
// starting from zero.
func hudRecording() {
	type recordingState struct {
		Type    string `json:"type"`
		Elapsed int    `json:"elapsed"`
	}
	data, _ := json.Marshal(recordingState{Type: "recording", Elapsed: 0})
	hudSetState(string(data))
}

// hudTranscribing switches the pill to the blue spinning-ring transcribing state.
func hudTranscribing() {
	type simpleState struct {
		Type string `json:"type"`
	}
	data, _ := json.Marshal(simpleState{Type: "transcribing"})
	hudSetState(string(data))
}

// hudError switches the pill to the red error state.  msg is shown as the
// second line below "Transcription failed".  The pill persists until the user
// clicks it (which calls shHudAction("dismiss")).
func hudError(msg string) {
	type errorState struct {
		Type    string `json:"type"`
		Message string `json:"message"`
	}
	data, _ := json.Marshal(errorState{Type: "error", Message: msg})
	hudSetState(string(data))
}

// hudHide slides the pill off-screen and orders the panel out.
func hudHide() { C.hud_hide_c() }

// hudSetState is the internal helper that marshals a JSON string to a C string
// and passes it to the ObjC hud_set_state() function.
func hudSetState(jsonStr string) {
	cs := C.CString(jsonStr)
	defer C.free(unsafe.Pointer(cs))
	C.hud_set_state(cs)
}

// shHudAction is called from ObjC (_SHHudDelegate) when the user interacts with
// the HUD.  Currently the only action is "dismiss" from the error state.
//
//export shHudAction
func shHudAction(action *C.char) {
	switch C.GoString(action) {
	case "dismiss":
		go hudHide()
	default:
		fmt.Printf("hud: unknown action %q\n", C.GoString(action))
	}
}
```

- [ ] **Step 2: Create `cmd/side-huddle/hud_other.go`**

Non-darwin stubs so the package compiles on other platforms. Also provides `gHotkeyCh` so `main.go`'s select loop compiles without a darwin build constraint.

```go
// cmd/side-huddle/hud_other.go
//go:build !darwin

package main

// gHotkeyCh mirrors the darwin declaration in hotkey_darwin.go so the
// main.go select loop compiles on non-darwin targets.
var gHotkeyCh = make(chan struct{}, 1)

// No-op HUD stubs — the HUD is macOS-only.
func hudWarmup()         {}
func hudRecording()      {}
func hudTranscribing()   {}
func hudError(_ string)  {}
func hudHide()           {}
```

- [ ] **Step 3: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0.

- [ ] **Step 4: Commit**

```bash
git add cmd/side-huddle/hud_darwin.go cmd/side-huddle/hud_other.go
git commit -m "feat(hud): add Go HUD bridge and non-darwin stubs"
```

---

## Task 5: Rename `offerTranscription` → `runTranscription` in `main.go`

**Files:**
- Modify: `cmd/side-huddle/main.go`

The current `offerTranscription` silently returns when `whisper-cli` or the model are absent. The renamed version returns an `error` so callers can show `hudError(err.Error())`. Internal stream-level transcription failures (individual WAV file errors) are still logged and do not bubble up as errors — only hard blockers (no binary, no model) fail the call.

- [ ] **Step 1: Rename the function signature and update early-return paths**

Replace the `offerTranscription` function signature and its two early-return cases. Find the function at approximately line 292 (`func offerTranscription`):

```go
// OLD signature:
func offerTranscription(ev *sh.Event, m meetingState, timeline []speakerEntry, recStart time.Time) {
	if _, err := exec.LookPath("whisper-cli"); err != nil {
		fmt.Println("(install whisper-cpp to enable local transcription: brew install whisper-cpp)")
		return
	}
	modelPath := whisperModelPath()
	if _, err := os.Stat(modelPath); err != nil {
		fmt.Printf("(whisper model not found at %s — download a .bin from huggingface.co/ggerganov/whisper.cpp)\n", modelPath)
		return
	}

// NEW signature:
func runTranscription(ev *sh.Event, m meetingState, timeline []speakerEntry, recStart time.Time) error {
	if _, err := exec.LookPath("whisper-cli"); err != nil {
		fmt.Println("(install whisper-cpp to enable local transcription: brew install whisper-cpp)")
		return fmt.Errorf("whisper-cli not found")
	}
	modelPath := whisperModelPath()
	if _, err := os.Stat(modelPath); err != nil {
		fmt.Printf("(whisper model not found at %s — download a .bin from huggingface.co/ggerganov/whisper.cpp)\n", modelPath)
		return fmt.Errorf("model not found: %s", filepath.Base(modelPath))
	}
```

- [ ] **Step 2: Add `return nil` at the end of `runTranscription`**

At the very end of the function body (after the `cocoaNotifyWithFolder("Transcript ready", ...)` call), replace the implicit void return with:

```go
	cocoaNotifyWithFolder("Transcript ready", meetingLabel, folder)
	return nil
}
```

- [ ] **Step 3: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: build error — `offerTranscription` is still called from the `RecordingReady` goroutine in `main.go`. This is expected; it will be fixed in Task 6.

- [ ] **Step 4: Update the call site in `main.go`**

Still in `main.go`, find the goroutine inside `case sh.RecordingReady:`:

```go
// OLD call site:
offerTranscription(&sh.Event{Path: capturedOrganized.Path, OthersPath: capturedOrganized.OthersPath, SelfPath: capturedOrganized.SelfPath}, capturedMeeting, capturedTimeline, capturedRecStart)

// NEW call site (return value temporarily ignored — will be handled in Task 6):
runTranscription(&sh.Event{Path: capturedOrganized.Path, OthersPath: capturedOrganized.OthersPath, SelfPath: capturedOrganized.SelfPath}, capturedMeeting, capturedTimeline, capturedRecStart) //nolint:errcheck
```

- [ ] **Step 5: Verify build passes**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0.

- [ ] **Step 6: Commit**

```bash
git add cmd/side-huddle/main.go
git commit -m "refactor(transcription): rename offerTranscription → runTranscription, return error"
```

---

## Task 6: Auto-transcription in the `RecordingReady` handler

**Files:**
- Modify: `cmd/side-huddle/main.go`

Replace the 120-second "Transcribe?" wait with immediate HUD-driven auto-transcription. The `durationSec` variable and `overlayRecordingSaved` / `waitOverlayPost` / `overlayTranscribing` / `shOverlayHide` calls are all removed. The goroutine now runs `hudTranscribing()` → `runTranscription()` → `hudError()` or `hudHide()`.

- [ ] **Step 1: Replace the `RecordingReady` block**

Find the `case sh.RecordingReady:` block (currently lines ~171–196) and replace it entirely with:

```go
		case sh.RecordingReady:
			organized := organizeRecording(e, meeting, baseDir)
			cocoaSetRecording(false, "", "") // defensive — already cleared on RecordingEnded
			fmt.Printf("💾  saved:\n")
			fmt.Printf("    mixed  → %s\n", organized.Path)
			fmt.Printf("    others → %s\n", organized.OthersPath)
			fmt.Printf("    self   → %s\n\n", organized.SelfPath)
			printTimeline(timeline, recordingStarted)
			// Snapshot state for the transcription goroutine — a new meeting
			// can start before transcription finishes, so we freeze the current
			// meeting's data here.
			capturedOrganized := organized
			capturedMeeting := meeting
			capturedTimeline := append([]speakerEntry(nil), timeline...)
			capturedRecStart := recordingStarted
			go func() {
				hudTranscribing()
				if err := runTranscription(
					&sh.Event{
						Path:       capturedOrganized.Path,
						OthersPath: capturedOrganized.OthersPath,
						SelfPath:   capturedOrganized.SelfPath,
					},
					capturedMeeting, capturedTimeline, capturedRecStart,
				); err != nil {
					hudError(err.Error())
				} else {
					hudHide()
				}
			}()
			timeline = timeline[:0] // reset for the next meeting
```

- [ ] **Step 2: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0. (`overlayRecordingSaved`, `waitOverlayPost`, `overlayTranscribing`, `shOverlayHide` are still defined in `overlay_darwin.go` — now dead code, which is fine for Go.)

- [ ] **Step 3: Commit**

```bash
git add cmd/side-huddle/main.go
git commit -m "feat(transcription): auto-transcribe on RecordingReady via HUD states"
```

---

## Task 7: Wire `hudRecording()` in `MeetingDetected` and add `hudWarmup()` at startup

**Files:**
- Modify: `cmd/side-huddle/main.go`

Two changes in `runListener`:
1. Call `hudWarmup()` at startup alongside `shOverlayWarmup()` so the first HUD show is instant.
2. Replace `overlayRecording(app)` with `hudRecording()` in the `MeetingDetected` goroutine.

- [ ] **Step 1: Add `hudWarmup()` call at startup**

In `runListener`, find the warmup line:

```go
	shOverlayWarmup() // pre-create panel so first show is instant
```

Replace it with:

```go
	shOverlayWarmup() // pre-create overlay panel so first show is instant
	hudWarmup()       // pre-create HUD panel so first show is instant
```

- [ ] **Step 2: Replace `overlayRecording(app)` with `hudRecording()`**

In the `MeetingDetected` goroutine, find:

```go
			overlayRecording(app)
			listener.Record()
```

Replace with:

```go
			hudRecording()
			listener.Record()
```

- [ ] **Step 3: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0.

- [ ] **Step 4: Commit**

```bash
git add cmd/side-huddle/main.go
git commit -m "feat(hud): wire hudRecording in MeetingDetected, add hudWarmup at startup"
```

---

## Task 8: Remove post-recording overlay code from `overlay_darwin.go` and `overlay_darwin.m`

**Files:**
- Modify: `cmd/side-huddle/overlay_darwin.go`
- Modify: `cmd/side-huddle/overlay_darwin.m`

All callers of the removed functions were deleted in Tasks 6 and 7. Now remove the dead code. The functions to remove are: `overlayRecording`, `overlayRecordingSaved`, `overlayTranscribing`, `waitOverlayPost`, and the `gOverlayPostCh` channel. Simplify `shOverlayAction` by dropping the `"transcribe"` and `"later"` cases, and removing the `gOverlayPostCh <- "dismiss"` send from the `"dismiss"` case.

- [ ] **Step 1: Rewrite `overlay_darwin.go`**

The complete new content of `cmd/side-huddle/overlay_darwin.go`:

```go
//go:build darwin && cgo

package main

/*
#cgo LDFLAGS: -framework WebKit

#include <stdlib.h>

void overlay_warmup(void);
void overlay_set_state(const char *jsonCStr);
void overlay_hide_c(void);
void overlay_set_mouse(int ignore);
void overlay_open(const char *pathCStr);
*/
import "C"
import (
	"encoding/json"
	"fmt"
	"time"
	"unsafe"
)

// overlayState is serialised to JSON and passed to the WKWebView setState() call.
type overlayState struct {
	Title   string          `json:"title"`
	Sub     string          `json:"sub,omitempty"`
	Dot     string          `json:"dot"`
	Timer   bool            `json:"timer,omitempty"`
	Buttons []overlayButton `json:"buttons,omitempty"`
}

type overlayButton struct {
	Label   string `json:"label"`
	Action  string `json:"a"`
	Primary bool   `json:"p,omitempty"`
}

func shOverlayWarmup() { C.overlay_warmup() }

func shOverlayShow(s overlayState) {
	data, _ := json.Marshal(s)
	cs := C.CString(string(data))
	defer C.free(unsafe.Pointer(cs))
	C.overlay_set_state(cs)
}

func shOverlayHide() { C.overlay_hide_c() }

// ── Action routing ────────────────────────────────────────────────────────────

// gOverlayRecordCh receives the user's record/skip choice (true=record).
// Capacity 1 so the delegate never blocks; stale values are drained before
// each new meeting in overlayMeetingDetected.
var gOverlayRecordCh = make(chan bool, 1)

// shOverlayAction is exported to C and called by _SHOverlayDelegate when a
// button is tapped inside the WebView.
//
//export shOverlayAction
func shOverlayAction(actionCStr *C.char) {
	action := C.GoString(actionCStr)
	switch action {
	case "record":
		select {
		case gOverlayRecordCh <- true:
		default:
		}
	case "dismiss":
		// dismiss before recording = skip the meeting
		select {
		case gOverlayRecordCh <- false:
		default:
		}
		go shOverlayHide()
	case "open_settings":
		cs := C.CString("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
		C.overlay_open(cs)
		C.free(unsafe.Pointer(cs))
	}
}

// ── Overlay state helpers ─────────────────────────────────────────────────────

func overlayMeetingDetected(app string) {
	// Drain stale record-choice from a prior meeting.
	select {
	case <-gOverlayRecordCh:
	default:
	}
	shOverlayShow(overlayState{
		Title: fmt.Sprintf("%s meeting detected", app),
		Sub:   "Record and transcribe?",
		Dot:   "green",
		Buttons: []overlayButton{
			{Label: "Record", Action: "record", Primary: true},
			{Label: "Dismiss", Action: "dismiss"},
		},
	})
}

// waitOverlayRecord blocks until the user taps Record or Dismiss in the overlay,
// or 60 s elapses (returns false — skip recording).
func waitOverlayRecord() bool {
	select {
	case v := <-gOverlayRecordCh:
		return v
	case <-time.After(60 * time.Second):
		go shOverlayHide()
		return false
	}
}
```

- [ ] **Step 2: Remove dead CSS from `overlay_darwin.m`**

In `overlay_darwin.m`, find the CSS rule:

```objc
".green{background:#30d158}.red{background:#ff453a;animation:p 1.2s ease-in-out infinite}"
".yellow{background:#ffd60a}.blue{background:#0a84ff}"
"@keyframes p{0%,100%{opacity:1;transform:scale(1)}50%{opacity:0.55;transform:scale(0.8)}}"
```

Replace with (keep only green, which is still used by `overlayMeetingDetected`):

```objc
".green{background:#30d158}"
```

- [ ] **Step 3: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0.

- [ ] **Step 4: Commit**

```bash
git add cmd/side-huddle/overlay_darwin.go cmd/side-huddle/overlay_darwin.m
git commit -m "refactor(overlay): remove post-recording states; HUD handles recording+transcribing"
```

---

## Task 9: Fix `gStopRecordingCh` drain and ad-hoc hotkey `for+select` loop

**Files:**
- Modify: `cmd/side-huddle/main.go`

Two bugs fixed in one task:
1. **Bug**: `gStopRecordingCh` (from `notify_darwin.go`) is sent to by the ObjC "Stop Recording" menu item but never drained in `runListener` — making the menu item a silent no-op.
2. **Missing feature**: The `gHotkeyCh` from `hotkey_darwin.go` is never read, so ⌥Space does nothing.

The final `select` is replaced by a `for+select` loop handling all three signals. For ad-hoc hotkey recording, `meeting` is reset with the current timestamp so `organizeRecording` generates a correct timestamp-based folder name (empty `app`/`title` fields → folder contains timestamp only).

- [ ] **Step 1: Replace the terminal `select` in `runListener`**

Find the final block of `runListener` (after `defer listener.Stop()`):

```go
	quit := make(chan os.Signal, 1)
	signal.Notify(quit, os.Interrupt, syscall.SIGTERM)

	// Keep the listener alive across meetings — this is a menu-bar agent, not
	// a one-shot. RecordingReady handles save + overlay inline; we just wait
	// here for ⌘Q (→ cocoaTerminate) or SIGINT to shut down cleanly.
	select {
	case <-quit:
		fmt.Println("\nshutting down…")
	}
```

Replace with:

```go
	quit := make(chan os.Signal, 1)
	signal.Notify(quit, os.Interrupt, syscall.SIGTERM)

	// adHocRecording tracks whether a hotkey-initiated recording is active.
	// Meeting-triggered recordings are managed by the listener event goroutine
	// above; the hotkey path runs entirely in this select loop.
	var adHocRecording bool

	for {
		select {
		case <-quit:
			fmt.Println("\nshutting down…")
			return

		case <-gStopRecordingCh:
			// "⏹ Stop Recording" menu-bar item — stops any active recording
			// (meeting-triggered or ad-hoc).
			listener.StopRecording()
			adHocRecording = false

		case <-gHotkeyCh:
			// ⌥Space toggle: stop if recording, start if idle.
			if adHocRecording {
				listener.StopRecording()
				adHocRecording = false
			} else {
				adHocRecording = true
				recordingStarted = time.Now()
				// Reset meeting so organizeRecording uses a clean timestamp.
				// Empty app+title → folder named by timestamp only.
				meeting = meetingState{
					started:          recordingStarted,
					participantsSeen: make(map[string]bool),
				}
				hudRecording()
				listener.Record()
			}
		}
	}
```

- [ ] **Step 2: Verify build**

```bash
cd /Users/ken/workspace/ms/side-huddle && go build ./...
```

Expected: no output, exit 0.

- [ ] **Step 3: Smoke-test the bundle (optional but recommended)**

```bash
cd /Users/ken/workspace/ms/side-huddle && make bundle && make install
```

Expected: app launches in menu bar. For manual verification:
- Open System Settings → Privacy & Security → Accessibility; grant access to SideHuddle.
- Press ⌥Space → green pill appears top-right with animated bars + elapsed timer.
- Press ⌥Space again → pill fades out, recording stops.
- Detect a Teams/Zoom meeting → "Record this meeting?" overlay appears as before.
- Tap "Record" → green HUD pill appears (no longer the full overlay recording card).
- After recording ends → blue "Transcribing" pill; on success pill fades; on error red pill with message.
- Click menu bar → "⏹ Stop Recording" item works (no longer silently ignored).

- [ ] **Step 4: Commit**

```bash
git add cmd/side-huddle/main.go
git commit -m "fix(hotkey): drain gStopRecordingCh; add ⌥Space ad-hoc recording toggle"
```

---

## Self-Review Checklist

**Spec coverage:**

| Spec requirement | Task |
|------------------|------|
| Persistent recording HUD pill (200×40pt, top-right, NSFloatingWindowLevel) | Task 3 |
| `recording` state: green #30d158, 3 animated bars, elapsed MM:SS | Task 3 |
| `transcribing` state: blue #0a84ff, spinning ring 14px/0.75s | Task 3 |
| `error` state: red #ff453a, ✕ glyph, two-line layout, clickable dismiss | Task 3 |
| Cross-fade/slide transitions via CSS | Task 3 |
| `hudRecording()`, `hudTranscribing()`, `hudError()`, `hudHide()`, `hudWarmup()` | Task 4 |
| `//export shHudAction` dismiss callback | Task 4 |
| Non-darwin stub `hud_other.go` | Task 4 |
| ⌥Space CGEventTap (keycode 49, Option-only) | Task 2 |
| Swallow event (return NULL) | Task 2 |
| `goHotkeyCallback` in separate file (CGo rule) | Task 1 |
| Accessibility check at launch + "Grant Accessibility Access…" menu item | Task 2 |
| Auto-transcription on `RecordingReady` (no prompt) | Task 6 |
| `offerTranscription` → `runTranscription` returning error | Task 5 |
| `hudTranscribing()` → `hudHide()` on success | Task 6 |
| `hudTranscribing()` → `hudError(msg)` on failure | Task 6 |
| Remove `overlayRecordingSaved`, `waitOverlayPost`, `gOverlayPostCh`, `overlayTranscribing` | Task 8 |
| Remove `"transcribe"`/`"later"` action cases from overlay | Task 8 |
| Remove `overlayRecording` (replaced by `hudRecording`) | Tasks 7, 8 |
| Bug fix: `gStopRecordingCh` drain in select loop | Task 9 |
| `for+select` loop with hotkey toggle | Task 9 |
| `meeting.started` set for ad-hoc (correct folder timestamp) | Task 9 |
| `hudWarmup()` at `runListener` startup | Task 7 |

**Type consistency check:**
- `hudSetState(string)` is the internal helper; only `hudRecording/hudTranscribing/hudError/hudHide` are exported to callers — consistent across Tasks 4, 6, 7, 9. ✓
- `gHotkeyCh chan struct{}` — defined in `hotkey_darwin.go` (darwin) and `hud_other.go` (!darwin); used in `main.go` Task 9. ✓
- `gStopRecordingCh chan struct{}` — defined in `notify_darwin.go`, drained in Task 9. ✓
- `runTranscription` signature `(ev *sh.Event, m meetingState, timeline []speakerEntry, recStart time.Time) error` — consistent in Tasks 5 and 6. ✓
- `shHudAction` exported name matches `extern void shHudAction(const char *action)` in `hud_darwin.m`. ✓
- `goHotkeyCallback` exported name matches the ObjC call in `cocoa_darwin.m`. ✓

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-06-recording-hud-hotkey.md`. Two execution options:

**1. Subagent-Driven (recommended)** — Fresh subagent per task, review between tasks, fast iteration.
Use superpowers:subagent-driven-development.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch with checkpoints.
Use superpowers:executing-plans.

Which approach?
