//! speaker_probe — detect active speaker in Teams using MeetingListener for
//! meeting/window detection (same audio-based logic as the core SDK) and
//! CGWindowListCreateImage + AX tree for pixel/name analysis.
//!
//! Usage:
//!   cargo run --example speaker_probe            # watch mode (default)
//!   cargo run --example speaker_probe -- --save  # single frame → /tmp/teams_frame.ppm

use std::collections::BTreeSet;
use std::ffi::{c_void, CString};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
use core_foundation_sys::base::{CFRelease, CFTypeRef};
use core_foundation_sys::string::{CFStringGetCString, CFStringRef, kCFStringEncodingUTF8};
use core_graphics::geometry::CGRect;

use side_huddle::window::{cg_window_owner, find_primary_window, window_bounds};
use side_huddle::{Event, MeetingListener};

// ── Log ───────────────────────────────────────────────────────────────────────

static LOG: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
const LOG_PATH: &str = "/tmp/speaker_probe.log";

fn log(msg: &str) {
    let now = chrono::Local::now().format("%H:%M:%S.%3f");
    if let Some(lock) = LOG.get() {
        if let Ok(mut f) = lock.lock() {
            let _ = writeln!(f, "[{now}] {msg}");
        }
    }
}

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

    // CGRectNull = (CGFLOAT_MAX/2, CGFLOAT_MAX/2, 0, 0)
    let null_rect = CGRect::new(
        &core_graphics::geometry::CGPoint { x: f64::MAX / 2.0, y: f64::MAX / 2.0 },
        &core_graphics::geometry::CGSize  { width: 0.0, height: 0.0 },
    );
    let f: Fn = unsafe { std::mem::transmute(ptr) };
    let img = unsafe { f(null_rect,
        1 << 3,  // kCGWindowListOptionIncludingWindow
        window_id,
        (1 << 0) | (1 << 2), // kCGWindowImageBoundsIgnoreFraming | kCGWindowImageNominalResolution
    )};
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

fn ax_find_tiles(pid: i32) -> (Vec<Tile>, usize) {
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

    // Walk inside an AXMenuItem → find AXStaticText with the clean display name
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

    unsafe fn recurse(el: AXUIElementRef, depth: usize, out: &mut Vec<Tile>, count: &mut usize) {
        *count += 1;
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
            return; // don't recurse into participant tiles
        }
        let (arr, kids) = ax_kids(el);
        for kid in kids { recurse(kid, depth+1, out, count); }
        if !arr.is_null() { CFRelease(arr as CFTypeRef); }
    }

    let mut tiles = Vec::new();
    let mut count = 0usize;
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if !app.is_null() {
            // AXEnhancedUserInterface tells Chromium to expose full web content AX tree.
            // Returns -25208 but still triggers the tree expansion (~261→1016+ nodes).
            use core_foundation_sys::number::kCFBooleanTrue;
            let attr = CFString::new("AXEnhancedUserInterface");
            AXUIElementSetAttributeValue(app, attr.as_concrete_TypeRef(), kCFBooleanTrue as CFTypeRef);
            recurse(app, 0, &mut tiles, &mut count);
            CFRelease(app as CFTypeRef);
        }
    }
    (tiles, count)
}

// ── Pixel analysis ────────────────────────────────────────────────────────────

fn is_saturated(r: u8, g: u8, b: u8) -> bool {
    let (r,g,b) = (r as i32, g as i32, b as i32);
    (r.max(g).max(b) - r.min(g).min(b)) > 35 && r.max(g).max(b) > 40
}

/// Sample chromatic pixels in a tile using two strategies:
/// 1. Outer border (for camera-ON tiles where the ring wraps the whole tile)
/// 2. Whole-tile scan at reduced density (for camera-OFF tiles where the ring
///    wraps the avatar circle in the tile center)
/// Returns (ratio, avg_hex) of the HIGHER chromatic region.
fn sample_border(px: &[u8], iw: i32, ih: i32, tx: f64, ty: f64, tw: f64, th: f64) -> (f64, String) {
    let x0=(tx as i32).max(0); let y0=(ty as i32).max(0);
    let x1=((tx+tw) as i32).min(iw-1); let y1=((ty+th) as i32).min(ih-1);
    if x1<=x0||y1<=y0 { return (0.0,"#000000".into()); }

    let mut sample = |x0: i32, y0: i32, x1: i32, y1: i32, step: i32| -> (f64, String) {
        let (mut rs,mut gs,mut bs,mut chroma,mut n)=(0i64,0i64,0i64,0i32,0i32);
        let mut y=y0; while y<=y1 {
            let mut x=x0; while x<=x1 {
                if x>=0&&x<iw&&y>=0&&y<ih {
                    let o=((y*iw+x)*4) as usize;
                    let(r,g,b)=(px[o],px[o+1],px[o+2]);
                    rs+=r as i64;gs+=g as i64;bs+=b as i64;
                    if is_saturated(r,g,b){chroma+=1;}
                    n+=1;
                }
                x+=step;
            }
            y+=step;
        }
        let hex=if n>0{format!("#{:02X}{:02X}{:02X}",rs/n as i64,gs/n as i64,bs/n as i64)}else{"#000000".into()};
        (if n>0{chroma as f64/n as f64}else{0.0},hex)
    };

    // Strategy 1: 8px outer border (camera-on ring)
    let (border_ratio, border_hex) = {
        let (mut rs,mut gs,mut bs,mut chroma,mut n)=(0i64,0i64,0i64,0i32,0i32);
        let mut s=|x:i32,y:i32|{
            if x<0||x>=iw||y<0||y>=ih{return;}
            let o=((y*iw+x)*4) as usize;
            let(r,g,b)=(px[o],px[o+1],px[o+2]);
            rs+=r as i64;gs+=g as i64;bs+=b as i64;
            if is_saturated(r,g,b){chroma+=1;}
            n+=1;
        };
        for bw in 0..8i32 {
            let mut x=x0; while x<=x1{s(x,y0+bw);s(x,y1-bw);x+=2;}
            let mut y=y0; while y<=y1{s(x0+bw,y);s(x1-bw,y);y+=2;}
        }
        let hex=if n>0{format!("#{:02X}{:02X}{:02X}",rs/n as i64,gs/n as i64,bs/n as i64)}else{"#000000".into()};
        (if n>0{chroma as f64/n as f64}else{0.0},hex)
    };

    // Strategy 2: sparse whole-tile scan every 4px (catches camera-off avatar ring anywhere in tile)
    let (full_ratio, full_hex) = sample(x0, y0, x1, y1, 4);

    // Return whichever strategy found more chromatic content
    if full_ratio > border_ratio { (full_ratio, full_hex) } else { (border_ratio, border_hex) }
}

// ── Snapshot ──────────────────────────────────────────────────────────────────

fn snapshot(app: &str, pid: u32, save: bool) -> Vec<String> {
    // Step 1: find participant tiles via AX (scans the whole app tree, independent of which window is visible)
    let ax_pid = if pid != 0 { pid as i32 } else {
        // fall back to exact-match pgrep if pid wasn't populated yet
        std::process::Command::new("pgrep").args(["-x", "MSTeams"]).output()
            .ok().and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.lines().next().and_then(|l| l.trim().parse().ok()))
            .unwrap_or(0)
    };
    log(&format!("ax_pid={ax_pid}"));
    let (tiles, node_count) = std::panic::catch_unwind(|| ax_find_tiles(ax_pid))
        .unwrap_or_else(|_| (vec![], 0));

    if tiles.is_empty() {
        log(&format!("tiles=0  ax_nodes={node_count}  (need ~1016; if low, AXEnhancedUserInterface not taking effect)"));
        return vec![];
    }

    // Step 2: find the actual meeting window (not Calendar, not Chat).
    // Window title is the reliable discriminator — Teams meeting windows have
    // the meeting/channel name first (e.g. "Bro talks to himself (General) | ...").
    let owner = cg_window_owner(app);
    let Some((win_id, wx, wy, ww, wh)) = find_meeting_window_for_capture(&owner) else {
        log("no capturable meeting window found"); return vec![];
    };
    // Filter tiles to only those whose center is inside this window.
    // Teams may expose tiles from multiple views (embedded + pop-out);
    // only the tiles within our window's bounds are meaningful.
    let in_window: Vec<&Tile> = tiles.iter().filter(|t| {
        let cx = t.x + t.w / 2.0;
        let cy = t.y + t.h / 2.0;
        cx >= wx && cx < wx+ww && cy >= wy && cy < wy+wh
    }).collect();

    log(&format!("window id={win_id}  {ww}x{wh} @{wx},{wy}  tiles_in_window={}/{}", in_window.len(), tiles.len()));

    if in_window.is_empty() {
        // No tiles in this window — the meeting view may be embedded elsewhere.
        // Log tile positions so we can see where they actually are.
        for t in &tiles {
            log(&format!("  tile '{}' at ({:.0},{:.0}) {:.0}x{:.0} — outside window", t.name, t.x, t.y, t.w, t.h));
        }
        return vec![];
    }

    let owned: Vec<Tile> = in_window.into_iter().map(|t| Tile {
        name: t.name.clone(), x: t.x, y: t.y, w: t.w, h: t.h
    }).collect();
    snapshot_with_window(win_id, wx, wy, ww, wh, &owned, node_count, save)
}

/// Find the best capturable Teams window for the meeting.
/// Priority: window whose title looks like a meeting (not Chat/Calendar/etc.)
/// Fall back to largest capturable Teams window if no titled meeting window found.
fn find_meeting_window_for_capture(owner: &str) -> Option<(u32, f64, f64, f64, f64)> {
    use core_graphics::window::{CGWindowListCopyWindowInfo, kCGNullWindowID, kCGWindowListOptionAll};
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
    use core_foundation_sys::dictionary::CFDictionaryRef;
    use core_foundation_sys::base::CFRelease;

    let list = unsafe { CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID) };
    if list.is_null() { return None; }
    let n = unsafe { CFArrayGetCount(list as _) };

    // Non-meeting titles to skip
    let skip = ["Calendar", "Chat", "Activity", "Calls", "OneDrive", "Teams NRC",
                "Microsoft Teams\0", "Viva", "Engage", "Copilot"];

    let mut meeting_win: Option<(u32, f64, f64, f64, f64)> = None; // titled meeting window
    let mut fallback_win: Option<(f64, u32, f64, f64, f64, f64)> = None; // largest capturable

    for i in 0..n {
        let item = unsafe { CFArrayGetValueAtIndex(list as _, i) } as CFDictionaryRef;
        if item.is_null() { continue; }
        if get_i32(item, "kCGWindowLayer") != 0 { continue; }
        let win_owner = get_str(item, "kCGWindowOwnerName").unwrap_or_default();
        if !win_owner.to_lowercase().contains(&owner.to_lowercase()) { continue; }

        let id = get_i32(item, "kCGWindowNumber") as u32;
        if id == 0 { continue; }
        let bd_key = CFString::new("kCGWindowBounds");
        let bd_val = unsafe { CFDictionaryGetValue(item, bd_key.as_concrete_TypeRef() as *const c_void) };
        if bd_val.is_null() { continue; }
        let bd = bd_val as CFDictionaryRef;
        let (x,y,w,h) = (get_f64(bd,"X"), get_f64(bd,"Y"), get_f64(bd,"Width"), get_f64(bd,"Height"));
        if w < 200.0 || h < 200.0 { continue; }

        let title = get_str(item, "kCGWindowName").unwrap_or_default();
        let is_meeting_title = !title.is_empty()
            && !skip.iter().any(|s| title.starts_with(s))
            && title != "Microsoft Teams";

        // Test captureability
        if let Some(img) = capture_window_cg(id) {
            unsafe { CGImageRelease(img); }
            if is_meeting_title && meeting_win.is_none() {
                meeting_win = Some((id, x, y, w, h));
            }
            let area = w * h;
            if fallback_win.as_ref().map_or(true, |(a,_,_,_,_,_)| area > *a) {
                fallback_win = Some((area, id, x, y, w, h));
            }
        }
    }
    unsafe { CFRelease(list as CFTypeRef); }

    meeting_win.or_else(|| fallback_win.map(|(_,id,x,y,w,h)| (id,x,y,w,h)))
}

fn snapshot_with_window(win_id: u32, wx: f64, wy: f64, ww: f64, wh: f64,
                         tiles: &[Tile], node_count: usize, save: bool) -> Vec<String> {
    let Some(img_ref) = capture_window_cg(win_id) else {
        log(&format!("capture returned null for id={win_id}")); return vec![];
    };
    let rgba = cg_image_to_rgba(img_ref);
    unsafe { CGImageRelease(img_ref); }
    let Some((pixels, img_w, img_h)) = rgba else {
        log("pixel extract failed"); return vec![];
    };
    if save {
        save_ppm(&pixels, img_w as u32, img_h as u32, "/tmp/teams_frame.ppm");
    }

    let sx = img_w as f64 / ww;
    let sy = img_h as f64 / wh;

    // Frame differencing — compare to previous frame to find what CHANGED.
    // A speaking ring will appear as new colored pixels; neutral background is stable.
    static PREV_FRAME: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
    let mut prev = PREV_FRAME.lock().unwrap();
    let diff_pixels: u32 = if prev.len() == pixels.len() {
        pixels.chunks(4).zip(prev.chunks(4)).map(|(cur, old)| {
            let (cr,cg,cb) = (cur[0] as i32, cur[1] as i32, cur[2] as i32);
            let (pr,pg,pb) = (old[0] as i32, old[1] as i32, old[2] as i32);
            let delta = ((cr-pr).abs() + (cg-pg).abs() + (cb-pb).abs()) as u32;
            // Count pixels that changed significantly AND are now chromatic
            if delta > 20 && is_saturated(cur[0], cur[1], cur[2]) { 1 } else { 0 }
        }).sum()
    } else { 0 };
    *prev = pixels.clone();
    drop(prev);

    let mut speakers = Vec::new();
    let mut tile_log = format!("tiles={}  ax_nodes={node_count}  diff_chroma_px={diff_pixels}",
                               tiles.len());

    for tile in tiles {
        let rx = (tile.x - wx) * sx;
        let ry = (tile.y - wy) * sy;
        let (ratio, hex) = sample_border(&pixels, img_w, img_h, rx, ry, tile.w * sx, tile.h * sy);

        // Trigger on EITHER: static chromatic ratio OR new colored pixels appearing (diff)
        let speaking = ratio > 0.02 || diff_pixels > 200;
        tile_log.push_str(&format!("\n  {} {:.1}% {} diff={diff_pixels} '{}'",
            if speaking {"🎤"} else {"  "}, ratio*100.0, hex, &tile.name[..tile.name.len().min(40)]));
        if save {
            println!("  {}  {}", if speaking {"🎤"} else {"  "}, &tile.name[..tile.name.len().min(50)]);
            println!("     {hex}  chromatic={:.1}%  diff_pixels={diff_pixels}", ratio*100.0);
        }
        if speaking { speakers.push(tile.name.clone()); }
    }
    log(&tile_log);
    speakers
}

// ── CGWindowList helpers (used by find_window_containing_tiles) ───────────────

use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_foundation_sys::number::{CFNumberGetValue, CFNumberRef,
    kCFNumberFloat64Type, kCFNumberSInt32Type};

fn get_i32(d: CFDictionaryRef, k: &str) -> i32 {
    let cf = CFString::new(k);
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return 0; }
    let mut n = 0i32;
    unsafe { CFNumberGetValue(v as CFNumberRef, kCFNumberSInt32Type, &mut n as *mut _ as *mut c_void); }
    n
}
fn get_f64(d: CFDictionaryRef, k: &str) -> f64 {
    let cf = CFString::new(k);
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return 0.0; }
    let mut n = 0f64;
    unsafe { CFNumberGetValue(v as CFNumberRef, kCFNumberFloat64Type, &mut n as *mut _ as *mut c_void); }
    n
}
fn get_str(d: CFDictionaryRef, k: &str) -> Option<String> {
    let cf = CFString::new(k);
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return None; }
    let mut buf = vec![0i8; 1024];
    let ok = unsafe { CFStringGetCString(v as CFStringRef, buf.as_mut_ptr(), 1024, kCFStringEncodingUTF8) };
    if ok != 0 { unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().ok().map(|s| s.to_owned()) } } else { None }
}

// ── PPM save ──────────────────────────────────────────────────────────────────

fn save_ppm(px: &[u8], w: u32, h: u32, path: &str) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "P6\n{} {}\n255\n", w, h).unwrap();
    for chunk in px.chunks(4) { f.write_all(&[chunk[0], chunk[1], chunk[2]]).unwrap(); }
    println!("Saved {path} ({w}×{h})");
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let save = args.iter().any(|a| a == "--save");

    // ── Init log ─────────────────────────────────────────────────────────────
    let log_file = OpenOptions::new().create(true).write(true).truncate(true)
        .open(LOG_PATH).expect("cannot open log file");
    LOG.set(Mutex::new(log_file)).ok();

    // ── Use MeetingListener for detection — same audio-based logic as the SDK ─
    // active_meeting holds (app_name, pid) when a meeting is in progress.
    let active: Arc<Mutex<Option<(String, u32)>>> = Arc::new(Mutex::new(None));
    let active_tx = Arc::clone(&active);
    let active_end = Arc::clone(&active);

    let listener = MeetingListener::new();
    listener.on(move |event| {
        match event {
            Event::MeetingDetected { app, pid } => {
                println!("📞 Meeting detected: {app}  pid={pid}");
                log(&format!("MeetingDetected app='{app}' pid={pid}"));
                *active_tx.lock().unwrap() = Some((app.clone(), *pid));
            }
            Event::MeetingEnded { app } => {
                println!("📴 Meeting ended: {app}");
                log(&format!("MeetingEnded app='{app}'"));
                *active_end.lock().unwrap() = None;
            }
            _ => {}
        }
    });
    listener.start().expect("failed to start MeetingListener");
    println!("Listening for meetings… (Ctrl-C to stop)");
    println!("Diagnostics → tail -f {LOG_PATH}\n");

    if save {
        // Single-shot: wait briefly for meeting detection then capture once
        std::thread::sleep(Duration::from_secs(3));
        let guard = active.lock().unwrap();
        if let Some((app, pid)) = guard.as_ref() {
            snapshot(app, *pid, true);
        } else {
            eprintln!("No active meeting detected after 3s — is Teams running with audio?");
        }
        return;
    }

    // ── Watch loop ────────────────────────────────────────────────────────────
    let mut last: BTreeSet<String> = BTreeSet::from(["__UNINIT__".into()]);
    loop {
        let meeting = active.lock().unwrap().clone();
        if let Some((app, pid)) = meeting {
            let cur: BTreeSet<String> = snapshot(&app, pid, false).into_iter().collect();
            if cur != last {
                let now = chrono::Local::now().format("%H:%M:%S.%3f");
                if cur.is_empty() {
                    println!("🔇 [{now}] silence\n");
                } else {
                    let names = cur.iter().cloned().collect::<Vec<_>>().join("  +  ");
                    println!("🎤 [{now}] {names}");
                    let joined: Vec<_> = cur.difference(&last).filter(|s| *s != "__UNINIT__").cloned().collect();
                    let left:   Vec<_> = last.difference(&cur).filter(|s| *s != "__UNINIT__").cloned().collect();
                    if !joined.is_empty() { println!("         ↑ joined:  {}", joined.join(", ")); }
                    if !left.is_empty()   { println!("         ↓ stopped: {}", left.join(", ")); }
                    println!();
                }
                last = cur;
            }
        } else {
            // Not in a meeting — reset state so first detection logs cleanly
            if last != BTreeSet::from(["__UNINIT__".into()]) {
                last = BTreeSet::from(["__UNINIT__".into()]);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
