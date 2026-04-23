// Slide-in overlay — ported from ~/workspace/ms/loom
// NSPanel + WKWebView; no UNUserNotificationCenter; works with or without a bundle.

#import <Cocoa/Cocoa.h>
#import <WebKit/WebKit.h>

extern void shOverlayAction(const char *action);

// ── Delegate ──────────────────────────────────────────────────────────────

@interface _SHOverlayDelegate : NSObject <WKScriptMessageHandler>
@end

@implementation _SHOverlayDelegate
- (void)userContentController:(WKUserContentController *)ucc
      didReceiveScriptMessage:(WKScriptMessage *)msg {
    NSString *action = (NSString *)msg.body ?: @"";
    shOverlayAction(action.UTF8String);
}
@end

// ── Module state ──────────────────────────────────────────────────────────

static NSPanel            *_gOverlay = nil;
static WKWebView          *_gWebView = nil;
static _SHOverlayDelegate *_gODel    = nil;

// ── HTML ──────────────────────────────────────────────────────────────────

static NSString *overlayHTML = @"<!DOCTYPE html>"
"<html><head><meta charset='utf-8'><style>"
"*{margin:0;padding:0;box-sizing:border-box}"
"html,body{background:transparent;width:100%;height:100%;overflow:hidden;"
"font-family:-apple-system,BlinkMacSystemFont,'SF Pro Text',sans-serif;"
"-webkit-font-smoothing:antialiased}"
"#card{position:absolute;top:12px;right:20px;left:20px;"
"background:rgba(28,28,30,0.93);"
"backdrop-filter:blur(24px) saturate(180%);"
"-webkit-backdrop-filter:blur(24px) saturate(180%);"
"border-radius:14px;border:0.5px solid rgba(255,255,255,0.1);"
"box-shadow:0 4px 20px rgba(0,0,0,0.45);"
"padding:13px 15px;"
"transform:translateX(120%) scale(0.95);opacity:0;"
"transition:transform 0.3s cubic-bezier(0.34,1.56,0.64,1),opacity 0.25s ease}"
"#card.on{transform:translateX(0) scale(1);opacity:1}"
".row{display:flex;align-items:center;gap:9px}"
".dot{width:8px;height:8px;border-radius:50%;flex-shrink:0}"
".green{background:#30d158}.red{background:#ff453a;animation:p 1.2s ease-in-out infinite}"
".yellow{background:#ffd60a}.blue{background:#0a84ff}"
"@keyframes p{0%,100%{opacity:1;transform:scale(1)}50%{opacity:0.55;transform:scale(0.8)}}"
".txt{flex:1;min-width:0}"
".t{color:rgba(255,255,255,0.95);font-size:13px;font-weight:600;line-height:1.3;"
"white-space:nowrap;overflow:hidden;text-overflow:ellipsis}"
".s{color:rgba(255,255,255,0.42);font-size:11px;margin-top:2px}"
".timer{font-variant-numeric:tabular-nums;color:rgba(255,255,255,0.45);font-size:12px;font-weight:500}"
".btns{display:flex;gap:6px;margin-top:10px}"
"button{flex:1;padding:6px 10px;border-radius:8px;font-size:12px;font-weight:500;"
"cursor:pointer;border:none;font-family:inherit;transition:opacity 0.1s}"
"button:active{opacity:0.7}"
".p{background:rgba(10,132,255,0.9);color:#fff}"
".sec{background:rgba(255,255,255,0.1);color:rgba(255,255,255,0.72)}"
"</style></head><body>"
"<div id='card'>"
"<div class='row'>"
"<div class='dot' id='dot'></div>"
"<div class='txt'><div class='t' id='ti'></div><div class='s' id='su'></div></div>"
"<div class='timer' id='tm'></div>"
"<button onclick=\"s('dismiss')\" style='flex:none;background:rgba(255,255,255,0.08);border:none;"
"border-radius:50%;width:20px;height:20px;color:rgba(255,255,255,0.4);cursor:pointer;"
"font-size:11px;display:flex;align-items:center;justify-content:center;padding:0'>\u2715</button>"
"</div>"
"<div class='btns' id='bt'></div></div>"
"<script>"
"var iv=null,sec=0;"
"function s(a){window.webkit.messageHandlers.sh.postMessage(a)}"
"function setState(d){"
"clearInterval(iv);iv=null;sec=0;"
"document.getElementById('tm').textContent='';"
"document.getElementById('bt').innerHTML='';"
"if(!d){document.getElementById('card').classList.remove('on');return}"
"document.getElementById('ti').textContent=d.title||'';"
"document.getElementById('su').textContent=d.sub||'';"
"document.getElementById('dot').className='dot '+(d.dot||'green');"
"if(d.timer){iv=setInterval(function(){"
"sec++;var m=Math.floor(sec/60),s=sec%60;"
"document.getElementById('tm').textContent=m+':'+(s<10?'0':'')+s"
"},1000)}"
"(d.buttons||[]).forEach(function(b){"
"var el=document.createElement('button');"
"el.textContent=b.label;el.className=b.p?'p':'sec';"
"el.onclick=function(){s(b.a)};document.getElementById('bt').appendChild(el)"
"});"
"document.getElementById('card').classList.add('on')"
"}"
"</script></body></html>";

// ── C API ──────────────────────────────────────────────────────────────────

void overlay_ensure_created(void) {
    if (_gOverlay) return;

    NSRect screen = [NSScreen mainScreen].visibleFrame;
    CGFloat w = 440, h = 152;
    NSRect frame = NSMakeRect(
        NSMaxX(screen) - w - 8,
        NSMaxY(screen) - h - 8,
        w, h
    );

    _gOverlay = [[NSPanel alloc]
        initWithContentRect:frame
        styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
        backing:NSBackingStoreBuffered
        defer:NO];

    _gOverlay.level               = NSFloatingWindowLevel;
    _gOverlay.opaque              = NO;
    _gOverlay.backgroundColor     = [NSColor clearColor];
    _gOverlay.hasShadow           = NO;
    // ignoresMouseEvents intentionally NOT set — NSNonactivatingPanel handles focus correctly
    _gOverlay.collectionBehavior  =
        NSWindowCollectionBehaviorCanJoinAllSpaces |
        NSWindowCollectionBehaviorStationary       |
        NSWindowCollectionBehaviorIgnoresCycle;
    [_gOverlay setAnimationBehavior:NSWindowAnimationBehaviorNone];

    // WKWebView
    WKWebViewConfiguration *cfg = [[WKWebViewConfiguration alloc] init];
    [cfg.userContentController addScriptMessageHandler:
        (_gODel = [[_SHOverlayDelegate alloc] init]) name:@"sh"];

    _gWebView = [[WKWebView alloc] initWithFrame:NSMakeRect(0, 0, w, h) configuration:cfg];
    [_gWebView setValue:@NO forKey:@"drawsBackground"];
    _gOverlay.contentView = _gWebView;

    [_gWebView loadHTMLString:overlayHTML baseURL:nil];
}

void overlay_set_state(const char *jsonCStr) {
    NSString *json = [NSString stringWithUTF8String:jsonCStr];
    dispatch_async(dispatch_get_main_queue(), ^{
        overlay_ensure_created();
        NSString *js = [NSString stringWithFormat:@"setState(%@)", json];
        [_gWebView evaluateJavaScript:js completionHandler:nil];
        [_gOverlay orderFrontRegardless];
    });
}

void overlay_warmup(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        overlay_ensure_created();
    });
}

void overlay_hide_c(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!_gOverlay) return;
        NSString *js = @"setState(null)";
        [_gWebView evaluateJavaScript:js completionHandler:nil];
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 300 * NSEC_PER_MSEC),
            dispatch_get_main_queue(), ^{
                [_gOverlay orderOut:nil];
            });
    });
}

void overlay_set_mouse(int ignore) {
    dispatch_async(dispatch_get_main_queue(), ^{
        _gOverlay.ignoresMouseEvents = (BOOL)ignore;
    });
}

void overlay_open(const char *pathCStr) {
    NSString *path = [NSString stringWithUTF8String:pathCStr];
    dispatch_async(dispatch_get_main_queue(), ^{
        [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:path]];
    });
}
