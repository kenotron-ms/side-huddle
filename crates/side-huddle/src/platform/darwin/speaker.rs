//! SpeakerWatcher — live speaker detection via CGWindowListCreateImage + AX tree.
//!
//! Polls every 100 ms while a meeting is active.  Uses CoreGraphics window capture
//! for pixel analysis (chromatic border ring detection) and the Accessibility API to
//! map each participant tile's screen position to a display name.  When the set of
//! detected speakers changes, calls the registered `on_change` closure.

use std::collections::BTreeSet;
use std::ffi::{c_void, CString};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
use core_foundation_sys::base::{CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_foundation_sys::number::{CFNumberGetValue, CFNumberRef, kCFNumberFloat64Type, kCFNumberSInt32Type};
use core_foundation_sys::string::{CFStringGetCString, CFStringRef, kCFStringEncodingUTF8};
use core_graphics::geometry::CGRect;

// ── CoreGraphics raw bindings ─────────────────────────────────────────────────

type CGImageRef      = *mut c_void;
type CGContextRef    = *mut c_void;
type CGColorSpaceRef = *mut c_void;

const KCG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
const KCG_BITMAP_BYTE_ORDER_32_BIG:       u32 = 4 << 12;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(img: CGImageRef)  -> usize;
    fn CGImageGetHeight(img: CGImageRef) -> usize;
    fn CGImageRelease(img: CGImageRef);
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(cs: CGColorSpaceRef);
    fn CGBitmapContextCreate(data: *mut u8, w: usize, h: usize,
        bpc: usize, bpr: usize, cs: CGColorSpaceRef, info: u32) -> CGContextRef;
    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, img: CGImageRef);
    fn CGContextRelease(ctx: CGContextRef);
}

// ── Window capture ────────────────────────────────────────────────────────────

/// CGWindowListCreateImage loaded via dlsym — bypasses the macOS 15+ header
/// "unavailable" annotation while the symbol still ships in CoreGraphics.dylib.
fn capture_window_cg(window_id: u32) -> Option<CGImageRef> {
    type Fn = unsafe extern "C" fn(CGRect, u32, u32, u32) -> CGImageRef;
    let sym = CString::new("CGWindowListCreateImage").unwrap();
    let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, sym.as_ptr()) };
    if ptr.is_null() { return None; }
    let null_rect = CGRect::new(
        &core_graphics::geometry::CGPoint { x: f64::MAX / 2.0, y: f64::MAX / 2.0 },
        &core_graphics::geometry::CGSize  { width: 0.0, height: 0.0 },
    );
    let f: Fn = unsafe { std::mem::transmute(ptr) };
    let img = unsafe { f(null_rect, 1 << 3, window_id, (1 << 0) | (1 << 2)) };
    if img.is_null() { None } else { Some(img) }
}

fn cg_image_to_rgba(img: CGImageRef) -> Option<(Vec<u8>, i32, i32)> {
    let w = unsafe { CGImageGetWidth(img) };
    let h = unsafe { CGImageGetHeight(img) };
    if w == 0 || h == 0 { return None; }
    let mut buf = vec![0u8; w * h * 4];
    let cs  = unsafe { CGColorSpaceCreateDeviceRGB() };
    let ctx = unsafe { CGBitmapContextCreate(buf.as_mut_ptr(), w, h, 8, w * 4, cs,
        KCG_IMAGE_ALPHA_PREMULTIPLIED_LAST | KCG_BITMAP_BYTE_ORDER_32_BIG) };
    if ctx.is_null() { unsafe { CGColorSpaceRelease(cs); } return None; }
    let rect = CGRect::new(
        &core_graphics::geometry::CGPoint { x: 0.0, y: 0.0 },
        &core_graphics::geometry::CGSize  { width: w as f64, height: h as f64 },
    );
    unsafe { CGContextDrawImage(ctx, rect, img); CGContextRelease(ctx); CGColorSpaceRelease(cs); }
    Some((buf, w as i32, h as i32))
}

// ── AX tile finder ────────────────────────────────────────────────────────────

struct Tile { name: String, x: f64, y: f64, w: f64, h: f64 }

fn strip_ax_state_suffixes(s: &str) -> String {
    let cut = [", muted", ", unmuted", ", speaking", ", not speaking", ", Context menu"];
    let mut end = s.len();
    for pat in &cut { if let Some(p) = s.find(pat) { end = end.min(p); } }
    s[..end].trim().to_owned()
}

fn ax_find_tiles(pid: i32) -> Vec<Tile> {
    type AXUIElementRef = *mut c_void;
    type AXValueRef     = *mut c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(el: AXUIElementRef, attr: CFStringRef,
                                         out: *mut CFTypeRef) -> i32;
        fn AXUIElementSetAttributeValue(el: AXUIElementRef, attr: CFStringRef,
                                         val: CFTypeRef) -> i32;
        fn AXValueGetValue(val: AXValueRef, ty: u32, out: *mut c_void) -> bool;
    }
    #[repr(C)] #[derive(Default,Copy,Clone)] struct Pt { x: f64, y: f64 }
    #[repr(C)] #[derive(Default,Copy,Clone)] struct Sz { w: f64, h: f64 }

    unsafe fn ax_str(el: AXUIElementRef, k: &str) -> Option<String> {
        let cf = CFString::new(k);
        let mut v: CFTypeRef = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut v) != 0 || v.is_null() { return None; }
        let mut buf = vec![0i8; 1024];
        let ok = CFStringGetCString(v as CFStringRef, buf.as_mut_ptr(), 1024, kCFStringEncodingUTF8);
        CFRelease(v);
        if ok != 0 { std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().ok().map(|s| s.to_owned()) } else { None }
    }
    unsafe fn ax_val<T: Default>(el: AXUIElementRef, k: &str, ty: u32) -> Option<T> {
        let cf = CFString::new(k);
        let mut v: CFTypeRef = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut v) != 0 || v.is_null() { return None; }
        let mut out = T::default();
        let ok = AXValueGetValue(v as AXValueRef, ty, &mut out as *mut T as *mut c_void);
        CFRelease(v);
        if ok { Some(out) } else { None }
    }
    unsafe fn ax_kids(el: AXUIElementRef) -> (core_foundation_sys::array::CFArrayRef, Vec<AXUIElementRef>) {
        let cf = CFString::new("AXChildren");
        let mut v: CFTypeRef = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut v) != 0 || v.is_null() {
            return (std::ptr::null(), vec![]);
        }
        let arr = v as core_foundation_sys::array::CFArrayRef;
        let kids = (0..CFArrayGetCount(arr)).map(|i| CFArrayGetValueAtIndex(arr, i) as AXUIElementRef).collect();
        (arr, kids)
    }
    unsafe fn find_name_text(el: AXUIElementRef, depth: usize) -> Option<String> {
        if depth > 8 { return None; }
        if ax_str(el, "AXRole").as_deref() == Some("AXStaticText") {
            if let Some(v) = ax_str(el, "AXValue") { if !v.is_empty() { return Some(v); } }
        }
        let (arr, kids) = ax_kids(el);
        let mut r = None;
        for kid in kids { if r.is_none() { r = find_name_text(kid, depth + 1); } }
        if !arr.is_null() { CFRelease(arr as CFTypeRef); }
        r
    }
    unsafe fn recurse(el: AXUIElementRef, depth: usize, out: &mut Vec<Tile>) {
        if depth >= 40 { return; }
        if ax_str(el, "AXRole").as_deref() == Some("AXMenuItem") {
            let name = find_name_text(el, 0).or_else(|| {
                let raw = ax_str(el, "AXTitle").filter(|s| !s.is_empty())
                    .or_else(|| ax_str(el, "AXDescription")).unwrap_or_default();
                let n = strip_ax_state_suffixes(&raw);
                if n.is_empty() { None } else { Some(n) }
            }).unwrap_or_default();
            if !name.is_empty() {
                if let (Some(p), Some(s)) = (ax_val::<Pt>(el,"AXPosition",1), ax_val::<Sz>(el,"AXSize",2)) {
                    if s.w > 10.0 && s.h > 10.0 { out.push(Tile { name, x:p.x, y:p.y, w:s.w, h:s.h }); }
                }
            }
            return;
        }
        let (arr, kids) = ax_kids(el);
        for kid in kids { recurse(kid, depth+1, out); }
        if !arr.is_null() { CFRelease(arr as CFTypeRef); }
    }

    let mut tiles = Vec::new();
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if !app.is_null() {
            use core_foundation_sys::number::kCFBooleanTrue;
            let attr = CFString::new("AXEnhancedUserInterface");
            AXUIElementSetAttributeValue(app, attr.as_concrete_TypeRef(), kCFBooleanTrue as CFTypeRef);
            recurse(app, 0, &mut tiles);
            CFRelease(app as CFTypeRef);
        }
    }
    tiles
}

// ── Pixel analysis ────────────────────────────────────────────────────────────

fn is_saturated(r: u8, g: u8, b: u8) -> bool {
    let (r,g,b) = (r as i32, g as i32, b as i32);
    (r.max(g).max(b) - r.min(g).min(b)) > 35 && r.max(g).max(b) > 40
}

/// Sample chromatic pixels using two strategies:
/// 1. 8px outer border (camera-ON ring around the whole tile)
/// 2. Sparse whole-tile scan every 4px (camera-OFF avatar ring)
/// Returns the ratio from whichever strategy found more chromatic content.
fn sample_border(px: &[u8], iw: i32, ih: i32, tx: f64, ty: f64, tw: f64, th: f64) -> f64 {
    let x0=(tx as i32).max(0); let y0=(ty as i32).max(0);
    let x1=((tx+tw) as i32).min(iw-1); let y1=((ty+th) as i32).min(ih-1);
    if x1<=x0||y1<=y0 { return 0.0; }

    // Strategy 1: 8px outer border
    let border_ratio = {
        let (mut chroma, mut n) = (0i32, 0i32);
        let mut s = |x:i32, y:i32| {
            if x<0||x>=iw||y<0||y>=ih { return; }
            let o=((y*iw+x)*4) as usize;
            if is_saturated(px[o],px[o+1],px[o+2]) { chroma+=1; }
            n+=1;
        };
        for bw in 0..8i32 {
            let mut x=x0; while x<=x1{s(x,y0+bw);s(x,y1-bw);x+=2;}
            let mut y=y0; while y<=y1{s(x0+bw,y);s(x1-bw,y);y+=2;}
        }
        if n>0 { chroma as f64/n as f64 } else { 0.0 }
    };

    // Strategy 2: sparse whole-tile scan every 4px
    let full_ratio = {
        let (mut chroma, mut n) = (0i32, 0i32);
        let mut y=y0; while y<=y1 {
            let mut x=x0; while x<=x1 {
                if x>=0&&x<iw&&y>=0&&y<ih {
                    let o=((y*iw+x)*4) as usize;
                    if is_saturated(px[o],px[o+1],px[o+2]) { chroma+=1; }
                    n+=1;
                }
                x+=4;
            }
            y+=4;
        }
        if n>0 { chroma as f64/n as f64 } else { 0.0 }
    };

    border_ratio.max(full_ratio)
}

// ── Window discovery ──────────────────────────────────────────────────────────

fn find_meeting_window(owner: &str) -> Option<(u32, f64, f64, f64, f64)> {
    use core_graphics::window::{CGWindowListCopyWindowInfo, kCGNullWindowID, kCGWindowListOptionAll};

    let skip = ["Calendar","Chat","Activity","Calls","OneDrive","Teams NRC",
                "Microsoft Teams\0","Viva","Engage","Copilot"];

    let list = unsafe { CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID) };
    if list.is_null() { return None; }
    let n = unsafe { CFArrayGetCount(list as _) };

    let mut meeting: Option<(u32,f64,f64,f64,f64)> = None;
    let mut fallback: Option<(f64,u32,f64,f64,f64,f64)> = None;

    for i in 0..n {
        let item = unsafe { CFArrayGetValueAtIndex(list as _, i) } as CFDictionaryRef;
        if item.is_null() { continue; }
        if cg_i32(item,"kCGWindowLayer") != 0 { continue; }
        let win_owner = cg_str(item,"kCGWindowOwnerName").unwrap_or_default();
        if !win_owner.to_lowercase().contains(&owner.to_lowercase()) { continue; }
        let id = cg_i32(item,"kCGWindowNumber") as u32;
        if id == 0 { continue; }
        let bd_key = CFString::new("kCGWindowBounds");
        let bd_val = unsafe { CFDictionaryGetValue(item, bd_key.as_concrete_TypeRef() as *const c_void) };
        if bd_val.is_null() { continue; }
        let bd = bd_val as CFDictionaryRef;
        let (x,y,w,h) = (cg_f64(bd,"X"),cg_f64(bd,"Y"),cg_f64(bd,"Width"),cg_f64(bd,"Height"));
        if w < 200.0 || h < 200.0 { continue; }
        let title = cg_str(item,"kCGWindowName").unwrap_or_default();
        let is_meeting = !title.is_empty()
            && !skip.iter().any(|s| title.starts_with(s))
            && title != "Microsoft Teams";
        if let Some(img) = capture_window_cg(id) {
            unsafe { CGImageRelease(img); }
            if is_meeting && meeting.is_none() { meeting = Some((id,x,y,w,h)); }
            let area = w*h;
            if fallback.as_ref().map_or(true,|(a,_,_,_,_,_)| area>*a) {
                fallback = Some((area,id,x,y,w,h));
            }
        }
    }
    unsafe { CFRelease(list as CFTypeRef); }
    meeting.or_else(|| fallback.map(|(_,id,x,y,w,h)| (id,x,y,w,h)))
}

fn cg_i32(d: CFDictionaryRef, k: &str) -> i32 {
    let cf = CFString::new(k);
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return 0; }
    let mut n = 0i32;
    unsafe { CFNumberGetValue(v as CFNumberRef, kCFNumberSInt32Type, &mut n as *mut _ as *mut c_void); }
    n
}
fn cg_f64(d: CFDictionaryRef, k: &str) -> f64 {
    let cf = CFString::new(k);
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return 0.0; }
    let mut n = 0f64;
    unsafe { CFNumberGetValue(v as CFNumberRef, kCFNumberFloat64Type, &mut n as *mut _ as *mut c_void); }
    n
}
fn cg_str(d: CFDictionaryRef, k: &str) -> Option<String> {
    let cf = CFString::new(k);
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return None; }
    let mut buf = vec![0i8; 1024];
    let ok = unsafe { CFStringGetCString(v as CFStringRef, buf.as_mut_ptr(), 1024, kCFStringEncodingUTF8) };
    if ok != 0 { unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().ok().map(|s| s.to_owned()) } } else { None }
}

// ── One probe frame ───────────────────────────────────────────────────────────

static PREV_FRAME: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

/// Capture one frame and return the names of currently-speaking participants.
pub(crate) fn probe_once(app: &str, pid: u32) -> Vec<String> {
    let owner = super::window::cg_window_owner(app);
    let Some((win_id, wx, wy, ww, wh)) = find_meeting_window(&owner) else { return vec![]; };

    let tiles = std::panic::catch_unwind(|| ax_find_tiles(pid as i32)).unwrap_or_default();
    if tiles.is_empty() { return vec![]; }

    let in_window: Vec<&Tile> = tiles.iter().filter(|t| {
        let cx = t.x + t.w / 2.0; let cy = t.y + t.h / 2.0;
        cx >= wx && cx < wx+ww && cy >= wy && cy < wy+wh
    }).collect();
    if in_window.is_empty() { return vec![]; }

    let Some(img_ref) = capture_window_cg(win_id) else { return vec![]; };
    let rgba = cg_image_to_rgba(img_ref);
    unsafe { CGImageRelease(img_ref); }
    let Some((pixels, img_w, img_h)) = rgba else { return vec![]; };

    let sx = img_w as f64 / ww;
    let sy = img_h as f64 / wh;

    // Frame diff: new chromatic pixels = ring appearing
    let mut prev = PREV_FRAME.lock().unwrap();
    let diff_px: u32 = if prev.len() == pixels.len() {
        pixels.chunks(4).zip(prev.chunks(4)).map(|(c,o)| {
            let delta = ((c[0] as i32-o[0] as i32).abs()
                       + (c[1] as i32-o[1] as i32).abs()
                       + (c[2] as i32-o[2] as i32).abs()) as u32;
            if delta > 20 && is_saturated(c[0],c[1],c[2]) { 1u32 } else { 0 }
        }).sum()
    } else { 0 };
    *prev = pixels.clone();
    drop(prev);

    let mut speakers = Vec::new();
    for tile in in_window {
        let rx = (tile.x - wx) * sx;
        let ry = (tile.y - wy) * sy;
        let ratio = sample_border(&pixels, img_w, img_h, rx, ry, tile.w * sx, tile.h * sy);
        if ratio > 0.02 || diff_px > 200 {
            speakers.push(tile.name.clone());
        }
    }
    speakers
}

// ── SpeakerWatcher ────────────────────────────────────────────────────────────

/// Polls speaker state every 100 ms and calls `on_change` whenever the set changes.
pub(crate) struct SpeakerWatcher {
    stop_tx: std::sync::mpsc::SyncSender<()>,
}

impl SpeakerWatcher {
    pub(crate) fn start(
        app: String,
        pid: u32,
        on_change: impl Fn(Vec<String>) + Send + 'static,
    ) -> Self {
        let (stop_tx, stop_rx) = sync_channel::<()>(1);
        std::thread::spawn(move || {
            let mut last: BTreeSet<String> = BTreeSet::new();
            loop {
                match stop_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                let cur: BTreeSet<String> = probe_once(&app, pid).into_iter().collect();
                if cur != last {
                    on_change(cur.iter().cloned().collect());
                    last = cur;
                }
            }
        });
        SpeakerWatcher { stop_tx }
    }
}

impl Drop for SpeakerWatcher {
    fn drop(&mut self) {
        let _ = self.stop_tx.try_send(());
    }
}
