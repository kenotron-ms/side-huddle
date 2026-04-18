//! speaker_probe — detect active speaker in Teams via window pixel analysis
//!
//! Uses CGWindowListCreateImage (loaded via dlsym to bypass the macOS 15+
//! "unavailable" header annotation) which composites the window's own
//! framebuffer regardless of what is in front of it.
//!
//! Usage:
//!   cargo run --example speaker_probe
//!   cargo run --example speaker_probe -- --save    # save /tmp/teams_frame.ppm
//!   cargo run --example speaker_probe -- --watch   # poll every 100ms

use std::collections::BTreeSet;
use std::ffi::{c_void, CString};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
use core_foundation_sys::base::{CFTypeRef, CFRelease};
use core_foundation_sys::dictionary::{CFDictionaryRef, CFDictionaryGetValue};
use core_foundation_sys::number::{CFNumberGetValue, CFNumberRef, kCFNumberFloat64Type, kCFNumberSInt32Type};
use core_foundation_sys::string::{CFStringGetCString, CFStringRef, kCFStringEncodingUTF8};
use core_graphics::geometry::CGRect;
use core_graphics::window::{kCGNullWindowID, kCGWindowListOptionAll, CGWindowListCopyWindowInfo};

// ── CoreGraphics raw bindings ─────────────────────────────────────────────────
//
// CGWindowListCreateImage IS in the dylib — it's only the header that says
// unavailable in macOS 15+. We load it via dlsym to bypass that annotation.
// Everything else (CGBitmapContextCreate etc.) is fine to declare directly.

type CGImageRef       = *mut c_void;
type CGContextRef     = *mut c_void;
type CGColorSpaceRef  = *mut c_void;
type CGWindowID       = u32;
type CGWindowListOption  = u32;
type CGWindowImageOption = u32;

const CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: CGWindowListOption  = 1 << 3;
const CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING:  CGWindowImageOption = 1 << 0;
const CG_WINDOW_IMAGE_NOMINAL_RESOLUTION:     CGWindowImageOption = 1 << 2;
const KCG_BITMAP_BYTE_ORDER_32_BIG:           u32 = 4 << 12;  // kCGBitmapByteOrder32Big = 16384
const KCG_IMAGE_ALPHA_PREMULTIPLIED_LAST:     u32 = 1;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(img: CGImageRef)  -> usize;
    fn CGImageGetHeight(img: CGImageRef) -> usize;
    fn CGImageRelease(img: CGImageRef);
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(cs: CGColorSpaceRef);
    fn CGBitmapContextCreate(
        data: *mut u8, width: usize, height: usize,
        bits_per_component: usize, bytes_per_row: usize,
        color_space: CGColorSpaceRef, bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, img: CGImageRef);
    fn CGContextRelease(ctx: CGContextRef);
}

/// Load and call CGWindowListCreateImage at runtime via dlsym.
/// CGRectNull = use the window's natural bounds.
fn capture_window_cg(window_id: CGWindowID) -> Option<CGImageRef> {
    type Fn = unsafe extern "C" fn(
        CGRect, CGWindowListOption, CGWindowID, CGWindowImageOption,
    ) -> CGImageRef;

    // CGRectNull on macOS = origin (CGFLOAT_MAX/2, CGFLOAT_MAX/2), size (0, 0)
    // Using f64::MAX/2 which matches CGFLOAT_MAX/2 on 64-bit
    let cg_rect_null = CGRect::new(
        &core_graphics::geometry::CGPoint { x: f64::MAX / 2.0, y: f64::MAX / 2.0 },
        &core_graphics::geometry::CGSize  { width: 0.0, height: 0.0 },
    );

    let sym = CString::new("CGWindowListCreateImage").unwrap();

    // Try RTLD_DEFAULT first, then explicit framework load as fallback
    let fn_ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, sym.as_ptr()) };
    let fn_ptr = if fn_ptr.is_null() {
        let lib_path = CString::new("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics").unwrap();
        let lib = unsafe { libc::dlopen(lib_path.as_ptr(), libc::RTLD_LAZY) };
        if lib.is_null() { eprintln!("dlopen CoreGraphics failed"); return None; }
        unsafe { libc::dlsym(lib, sym.as_ptr()) }
    } else {
        fn_ptr
    };
    if fn_ptr.is_null() { eprintln!("CGWindowListCreateImage not found via dlsym"); return None; }

    let f: Fn = unsafe { std::mem::transmute(fn_ptr) };
    let img = unsafe { f(
        cg_rect_null,
        CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
        window_id,
        CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING | CG_WINDOW_IMAGE_NOMINAL_RESOLUTION,
    )};
    if img.is_null() { None } else { Some(img) }
}

/// Render CGImage into an RGBA8 byte vector.
fn cg_image_to_rgba(img: CGImageRef) -> Option<(Vec<u8>, i32, i32)> {
    let w = unsafe { CGImageGetWidth(img) };
    let h = unsafe { CGImageGetHeight(img) };
    if w == 0 || h == 0 { return None; }
    let mut buf = vec![0u8; w * h * 4];
    let cs  = unsafe { CGColorSpaceCreateDeviceRGB() };
    let ctx = unsafe { CGBitmapContextCreate(
        buf.as_mut_ptr(), w, h, 8, w * 4, cs,
        KCG_IMAGE_ALPHA_PREMULTIPLIED_LAST | KCG_BITMAP_BYTE_ORDER_32_BIG,
    )};
    if ctx.is_null() { unsafe { CGColorSpaceRelease(cs); } return None; }
    let rect = CGRect::new(
        &core_graphics::geometry::CGPoint { x: 0.0, y: 0.0 },
        &core_graphics::geometry::CGSize  { width: w as f64, height: h as f64 },
    );
    unsafe { CGContextDrawImage(ctx, rect, img); }
    unsafe { CGContextRelease(ctx); CGColorSpaceRelease(cs); }
    Some((buf, w as i32, h as i32))
}

// ── Window finder ─────────────────────────────────────────────────────────────

struct WinInfo { id: u32, x: f64, y: f64, w: f64, h: f64 }

fn find_meeting_window(owner_pid: i32) -> Option<WinInfo> {
    let list = unsafe { CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID) };
    if list.is_null() { return None; }
    let n = unsafe { CFArrayGetCount(list as _) };
    let mut best: Option<(f64, WinInfo)> = None;

    for i in 0..n {
        let item = unsafe { CFArrayGetValueAtIndex(list as _, i) } as CFDictionaryRef;
        if item.is_null() { continue; }
        if get_i32(item, "kCGWindowLayer") != 0 { continue; }

        // Must match the main Teams PID — filters out WebView subprocess windows
        // which CGWindowListCreateImage cannot capture
        let win_pid = get_i32(item, "kCGWindowOwnerPID");
        if win_pid != owner_pid { continue; }

        let bd_key = CFString::new("kCGWindowBounds");
        let bd_val = unsafe { CFDictionaryGetValue(item, bd_key.as_concrete_TypeRef() as *const c_void) };
        if bd_val.is_null() { continue; }
        let bd = bd_val as CFDictionaryRef;
        let (x, y, w, h) = (get_f64(bd,"X"), get_f64(bd,"Y"), get_f64(bd,"Width"), get_f64(bd,"Height"));
        let area = w * h;
        if area < 10_000.0 { continue; }
        let id = get_i32(item, "kCGWindowNumber") as u32;
        if id == 0 { continue; }
        if best.as_ref().map_or(true, |(a,_)| area > *a) {
            best = Some((area, WinInfo { id, x, y, w, h }));
        }
    }
    unsafe { CFRelease(list as CFTypeRef); }
    best.map(|(_,w)| w)
}

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

// ── Snapshot ──────────────────────────────────────────────────────────────────

fn snapshot(pid: i32, save: bool) -> Vec<String> {
    let win = match find_meeting_window(pid) {
        Some(w) => w,
        None => { eprintln!("No Teams window found for pid={pid}"); return vec![]; }
    };
    let img_ref = match capture_window_cg(win.id) {
        Some(r) => r,
        None => { eprintln!("Capture failed"); return vec![]; }
    };
    let result = cg_image_to_rgba(img_ref);
    unsafe { CGImageRelease(img_ref); }
    let (pixels, img_w, img_h) = match result {
        Some(r) => r,
        None => { eprintln!("Pixel extract failed"); return vec![]; }
    };

    if save { save_ppm(&pixels, img_w as u32, img_h as u32, "/tmp/teams_frame.ppm"); }

    let tiles = std::panic::catch_unwind(|| ax_find_tiles(pid)).unwrap_or_default();
    if tiles.is_empty() {
        if save { eprintln!("No AXMenuItem tiles — are you in a meeting?"); }
        return vec![];
    }

    let scale_x = img_w as f64 / win.w;
    let scale_y = img_h as f64 / win.h;

    // In single-shot mode print the full diagnostic table; in watch mode stay quiet
    if save {
        let now = chrono::Local::now().format("%H:%M:%S");
        println!("[{now}] {} tiles  {}×{}  win@({:.0},{:.0} {}×{})",
                 tiles.len(), img_w, img_h, win.x, win.y, win.w as i32, win.h as i32);
    }

    // Collect ALL currently-speaking tiles (there can be more than one)
    let mut speakers: Vec<String> = Vec::new();
    for tile in &tiles {
        let rx = (tile.x - win.x) * scale_x;
        let ry = (tile.y - win.y) * scale_y;
        let (ratio, hex) = sample_border(&pixels, img_w, img_h, rx, ry, tile.w * scale_x, tile.h * scale_y);
        let speaking = ratio > 0.15;
        if save {
            println!("  {}  {}", if speaking {"🎤"} else {"  "}, &tile.name[..tile.name.len().min(50)]);
            println!("     {hex}  chromatic={:.0}%", ratio * 100.0);
        }
        if speaking { speakers.push(tile.name.clone()); }
    }
    speakers
}

// ── Pixel analysis ────────────────────────────────────────────────────────────

fn is_saturated(r: u8, g: u8, b: u8) -> bool {
    let (r,g,b) = (r as i32, g as i32, b as i32);
    (r.max(g).max(b) - r.min(g).min(b)) > 35 && r.max(g).max(b) > 40
}

fn sample_border(px: &[u8], iw: i32, ih: i32, tx: f64, ty: f64, tw: f64, th: f64) -> (f64, String) {
    let x0 = (tx as i32).max(0); let y0 = (ty as i32).max(0);
    let x1 = ((tx+tw) as i32).min(iw-1); let y1 = ((ty+th) as i32).min(ih-1);
    if x1 <= x0 || y1 <= y0 { return (0.0, "#000000".into()); }
    let (mut rs, mut gs, mut bs, mut chroma, mut n) = (0i64, 0i64, 0i64, 0i32, 0i32);
    let mut s = |x: i32, y: i32| {
        if x<0||x>=iw||y<0||y>=ih { return; }
        let o = ((y*iw+x)*4) as usize;
        let (r,g,b) = (px[o],px[o+1],px[o+2]);
        rs+=r as i64; gs+=g as i64; bs+=b as i64;
        if is_saturated(r,g,b) { chroma+=1; }
        n+=1;
    };
    for bw in 0..4i32 {
        let mut x=x0; while x<=x1 { s(x,y0+bw); s(x,y1-bw); x+=2; }
        let mut y=y0; while y<=y1 { s(x0+bw,y); s(x1-bw,y); y+=2; }
    }
    let hex = if n>0 { format!("#{:02X}{:02X}{:02X}", rs/n as i64, gs/n as i64, bs/n as i64) } else { "#000000".into() };
    (if n>0 { chroma as f64/n as f64 } else { 0.0 }, hex)
}

// ── AX tile finder ────────────────────────────────────────────────────────────

struct Tile { name: String, x: f64, y: f64, w: f64, h: f64 }

fn ax_find_tiles(pid: i32) -> Vec<Tile> {
    type AXUIElementRef = *mut c_void;
    type AXValueRef     = *mut c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(el: AXUIElementRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
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
        // Don't release the array — caller releases it after using children
        let kids: Vec<_> = (0..CFArrayGetCount(arr))
            .map(|i| CFArrayGetValueAtIndex(arr, i) as AXUIElementRef)
            .collect();
        (arr, kids)
    }
    unsafe fn recurse(el: AXUIElementRef, depth: usize, out: &mut Vec<Tile>) {
        if depth > 25 { return; }
        if ax_str(el, "AXRole").as_deref() == Some("AXMenuItem") {
            let name = ax_str(el, "AXTitle").filter(|s| !s.is_empty())
                .or_else(|| ax_str(el, "AXDescription")).unwrap_or_else(|| "(unnamed)".into());
            if let (Some(p), Some(s)) = (ax_val::<Pt>(el,"AXPosition",1), ax_val::<Sz>(el,"AXSize",2)) {
                if s.w > 10.0 && s.h > 10.0 { out.push(Tile { name, x:p.x, y:p.y, w:s.w, h:s.h }); }
            }
        }
        let (arr, kids) = ax_kids(el);
        for kid in kids { recurse(kid, depth+1, out); }
        if !arr.is_null() { CFRelease(arr as CFTypeRef); }
    }
    let mut tiles = Vec::new();
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if !app.is_null() { recurse(app, 0, &mut tiles); CFRelease(app as CFTypeRef); }
    }
    tiles
}

// ── PPM save ──────────────────────────────────────────────────────────────────

fn save_ppm(px: &[u8], w: u32, h: u32, path: &str) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "P6\n{} {}\n255\n", w, h).unwrap();
    for chunk in px.chunks(4) { f.write_all(&[chunk[0], chunk[1], chunk[2]]).unwrap(); }
    println!("Saved {path} ({w}×{h}) — open with Preview");
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn find_teams_pid() -> Option<i32> {
    for name in &["MSTeams", "Microsoft Teams"] {
        if let Ok(out) = std::process::Command::new("pgrep").arg(name).output() {
            if let Some(pid) = std::str::from_utf8(&out.stdout).ok()
                .and_then(|s| s.lines().next())
                .and_then(|l| l.trim().parse().ok()) { return Some(pid); }
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let watch = args.iter().any(|a| a == "--watch");
    let save  = args.iter().any(|a| a == "--save");
    let pid = args.iter().find(|a| a.parse::<i32>().is_ok())
        .and_then(|a| a.parse().ok())
        .or_else(find_teams_pid)
        .unwrap_or_else(|| { eprintln!("Microsoft Teams not running"); std::process::exit(1); });
    println!("Microsoft Teams PID {pid}");

    if watch {
        println!("Watching for speaker changes (Ctrl-C to stop)…\n");
        // Use a sentinel that can never match real output so first frame always logs
        let mut last: BTreeSet<String> = BTreeSet::from(["__UNINIT__".into()]);

        loop {
            let cur: BTreeSet<String> = snapshot(pid, false).into_iter().collect();

            if cur != last {
                let now = chrono::Local::now().format("%H:%M:%S.%3f");

                if cur.is_empty() {
                    println!("🔇 [{now}] silence");
                } else {
                    // Who joined / who left the speaking set this frame
                    let joined: Vec<_> = cur.difference(&last).cloned().collect();
                    let left:   Vec<_> = last.difference(&cur).cloned().collect();

                    // Print the full current speaking set
                    let names = cur.iter().cloned().collect::<Vec<_>>().join("  +  ");
                    println!("🎤 [{now}] {names}");

                    // Annotate transitions on the same line suffix
                    if !joined.is_empty() && last.len() > 0 && !last.contains("__UNINIT__") {
                        println!("         ↑ joined:  {}", joined.join(", "));
                    }
                    if !left.is_empty() {
                        println!("         ↓ stopped: {}", left.join(", "));
                    }
                }
                println!();
                last = cur;
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    } else {
        snapshot(pid, save);
    }
}
