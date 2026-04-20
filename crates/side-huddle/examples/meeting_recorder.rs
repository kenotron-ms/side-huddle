//! meeting_recorder — full diarisation pipeline
//!
//! Combines three streams into a single VTT + WAV output:
//!
//!  1. System audio tap  ──┐
//!  2. Microphone audio  ──┴─ MeetingListener.record() ──► combined WAV
//!  3. Speaker tiles (AX + pixel detection) ──────────────► speaker timeline
//!
//! After the meeting ends:
//!  WAV + speaker timeline ──► Whisper ──► per-segment text + timestamps
//!                          ──► attribute each segment to a speaker
//!                          ──► write   <title>.vtt  (WebVTT)
//!                          ──► write   <title>.wav  (already from recorder)
//!
//! Configuration (environment variables):
//!   WHISPER_URL   – Whisper-compatible endpoint (default: http://localhost:8080/v1/audio/transcriptions)
//!   OPENAI_API_KEY – Bearer token (required for OpenAI, optional for local servers)
//!   MEETINGS_DIR   – output directory (default: ~/Documents/meetings)

use std::collections::BTreeSet;
use std::ffi::{c_void, CString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
use core_foundation_sys::base::{CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_foundation_sys::number::{
    CFNumberGetValue, CFNumberRef, kCFNumberFloat64Type, kCFNumberSInt32Type,
};
use core_foundation_sys::string::{CFStringGetCString, CFStringRef, kCFStringEncodingUTF8};
use core_graphics::geometry::CGRect;

use side_huddle::window::{cg_window_owner, find_primary_window};
use side_huddle::{Event, MeetingListener};

// ── Speaker timeline ──────────────────────────────────────────────────────────

/// A point-in-time observation: who (if anyone) was visually detected speaking.
#[derive(Debug, Clone)]
struct SpeakerEvent {
    /// Seconds since recording started (wall-clock offset).
    offset_secs: f64,
    /// Visual ring detection result — empty = nobody / "Me" speaking.
    speakers: Vec<String>,
}

#[derive(Default)]
struct Timeline {
    started_at: Option<Instant>,
    events:     Vec<SpeakerEvent>,
    /// All participant names seen so far (for VTT speaker cue blocks).
    seen:       BTreeSet<String>,
}

impl Timeline {
    fn push(&mut self, speakers: Vec<String>) {
        let offset = self.started_at.map_or(0.0, |t| t.elapsed().as_secs_f64());
        self.seen.extend(speakers.iter().cloned());
        self.events.push(SpeakerEvent { offset_secs: offset, speakers });
    }

    /// Returns the speaker name (or "Me") active nearest `at` seconds,
    /// searching within a ±500 ms window. Returns None when there is no
    /// detection within that window (i.e. genuine silence gap).
    fn speaker_at(&self, at: f64) -> Option<String> {
        let window = 0.5_f64;
        self.events
            .iter()
            .filter(|e| (e.offset_secs - at).abs() <= window)
            .min_by(|a, b| {
                (a.offset_secs - at)
                    .abs()
                    .partial_cmp(&(b.offset_secs - at).abs())
                    .unwrap()
            })
            .map(|e| {
                if e.speakers.is_empty() {
                    "Me".to_string()
                } else {
                    e.speakers.join(" + ")
                }
            })
    }
}

// ── Whisper client ────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Segment {
    start:   f64,
    end:     f64,
    text:    String,
    /// Pre-assigned speaker (e.g. "Me" for mic stream); None = attribute from timeline
    speaker: Option<String>,
}

/// Send a WAV file to a Whisper-compatible endpoint and return raw segments.
/// Uses `curl` — works with OpenAI, local faster-whisper, Groq, etc.
fn transcribe_with(wav_path: &Path, url: &str, api_key: &str, label: &str) -> Vec<Segment> {
    println!("  ↑ [{label}] {}", wav_path.file_name().unwrap_or_default().to_string_lossy());
    let file_arg = format!("file=@{};type=audio/wav", wav_path.display());
    let output = std::process::Command::new("curl")
        .args([
            "-sX", "POST", url,
            "-H", &format!("Authorization: Bearer {api_key}"),
            "-F", &file_arg,
            "-F", "model=whisper-1",
            "-F", "response_format=verbose_json",
            "-F", "timestamp_granularities[]=segment",
        ])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => { eprintln!("  curl error [{label}]: {e}"); return vec![]; }
    };
    if !output.status.success() {
        eprintln!("  Whisper error [{label}] ({}): {}",
                  output.status, String::from_utf8_lossy(&output.stderr));
        return vec![];
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(j) => j,
        Err(e) => { eprintln!("  JSON parse error [{label}]: {e}"); return vec![]; }
    };
    json["segments"].as_array().map(|arr| {
        arr.iter().filter_map(|s| Some(Segment {
            start:   s["start"].as_f64()?,
            end:     s["end"].as_f64()?,
            text:    s["text"].as_str()?.trim().to_owned(),
            speaker: None,
        })).collect()
    }).unwrap_or_default()
}

// ── VTT writer ────────────────────────────────────────────────────────────────

fn secs_to_vtt(s: f64) -> String {
    let ms  = (s * 1000.0) as u64;
    let h   = ms / 3_600_000;
    let m   = (ms % 3_600_000) / 60_000;
    let sec = (ms % 60_000) / 1000;
    let ms  = ms % 1000;
    format!("{h:02}:{m:02}:{sec:02}.{ms:03}")
}

fn write_vtt_segs(segments: &[Segment], timeline: &Timeline, path: &Path) {
    let mut f = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => { eprintln!("Cannot write VTT: {e}"); return; }
    };
    writeln!(f, "WEBVTT").ok();
    writeln!(f, "").ok();
    for name in &timeline.seen { writeln!(f, "NOTE speaker: {name}").ok(); }
    writeln!(f, "NOTE speaker: Me").ok();
    writeln!(f, "").ok();
    for (i, seg) in segments.iter().enumerate() {
        let speaker = seg.speaker.clone().unwrap_or_else(|| {
            let mid = (seg.start + seg.end) / 2.0;
            timeline.speaker_at(mid).unwrap_or_else(|| "Unknown".into())
        });
        writeln!(f, "{}", i + 1).ok();
        writeln!(f, "{} --> {}", secs_to_vtt(seg.start), secs_to_vtt(seg.end)).ok();
        writeln!(f, "<v {speaker}>{}", seg.text).ok();
        writeln!(f, "").ok();
    }
}

// ── Shared session state ──────────────────────────────────────────────────────

struct SessionState {
    app:      String,
    pid:      u32,
    timeline: Timeline,
    running:  bool,
}

// ── Probe helpers (AX + pixel detection) ─────────────────────────────────────
// All of this is the same CGWindow / AX machinery as speaker_probe.rs.

type CGImageRef      = *mut c_void;
type CGContextRef    = *mut c_void;
type CGColorSpaceRef = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(img: CGImageRef)  -> usize;
    fn CGImageGetHeight(img: CGImageRef) -> usize;
    fn CGImageRelease(img: CGImageRef);
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(cs: CGColorSpaceRef);
    fn CGBitmapContextCreate(
        data: *mut u8, w: usize, h: usize, bpc: usize,
        bpr: usize, cs: CGColorSpaceRef, info: u32,
    ) -> CGContextRef;
    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, img: CGImageRef);
    fn CGContextRelease(ctx: CGContextRef);
}

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
    let (w, h) = unsafe { (CGImageGetWidth(img), CGImageGetHeight(img)) };
    if w == 0 || h == 0 { return None; }
    let mut buf = vec![0u8; w * h * 4];
    let cs  = unsafe { CGColorSpaceCreateDeviceRGB() };
    let ctx = unsafe {
        CGBitmapContextCreate(buf.as_mut_ptr(), w, h, 8, w * 4, cs,
            1 /* kCGImageAlphaPremultipliedLast */ | (4 << 12) /* kCGBitmapByteOrder32Big */)
    };
    if ctx.is_null() { unsafe { CGColorSpaceRelease(cs); } return None; }
    let rect = CGRect::new(
        &core_graphics::geometry::CGPoint { x: 0.0, y: 0.0 },
        &core_graphics::geometry::CGSize  { width: w as f64, height: h as f64 },
    );
    unsafe { CGContextDrawImage(ctx, rect, img); CGContextRelease(ctx); CGColorSpaceRelease(cs); }
    Some((buf, w as i32, h as i32))
}

struct Tile { name: String, x: f64, y: f64, w: f64, h: f64 }

fn strip_ax_state(s: &str) -> String {
    let cut = [", muted", ", unmuted", ", speaking", ", not speaking", ", Context menu"];
    let mut end = s.len();
    for p in &cut { if let Some(i) = s.find(p) { end = end.min(i); } }
    s[..end].trim().to_owned()
}

fn ax_find_tiles(pid: i32) -> Vec<Tile> {
    type AXRef  = *mut c_void;
    type AXValRef = *mut c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXRef;
        fn AXUIElementCopyAttributeValue(el: AXRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
        fn AXUIElementSetAttributeValue(el: AXRef, attr: CFStringRef, val: CFTypeRef) -> i32;
        fn AXValueGetValue(val: AXValRef, ty: u32, out: *mut c_void) -> bool;
    }
    #[repr(C)] #[derive(Default,Copy,Clone)] struct Pt { x: f64, y: f64 }
    #[repr(C)] #[derive(Default,Copy,Clone)] struct Sz { w: f64, h: f64 }

    unsafe fn ax_str(el: AXRef, k: &str) -> Option<String> {
        let cf = CFString::new(k);
        let mut v: CFTypeRef = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut v) != 0 || v.is_null() { return None; }
        let mut buf = vec![0i8; 1024];
        let ok = CFStringGetCString(v as CFStringRef, buf.as_mut_ptr(), 1024, kCFStringEncodingUTF8);
        CFRelease(v);
        if ok != 0 { std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().ok().map(|s| s.to_owned()) } else { None }
    }
    unsafe fn ax_val<T: Default>(el: AXRef, k: &str, ty: u32) -> Option<T> {
        let cf = CFString::new(k);
        let mut v: CFTypeRef = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut v) != 0 || v.is_null() { return None; }
        let mut out = T::default();
        let ok = AXValueGetValue(v as AXValRef, ty, &mut out as *mut T as *mut c_void);
        CFRelease(v);
        if ok { Some(out) } else { None }
    }
    unsafe fn ax_kids(el: AXRef) -> (core_foundation_sys::array::CFArrayRef, Vec<AXRef>) {
        let cf = CFString::new("AXChildren");
        let mut v: CFTypeRef = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut v) != 0 || v.is_null() {
            return (std::ptr::null(), vec![]);
        }
        let arr = v as core_foundation_sys::array::CFArrayRef;
        let kids = (0..CFArrayGetCount(arr))
            .map(|i| CFArrayGetValueAtIndex(arr, i) as AXRef)
            .collect();
        (arr, kids)
    }
    unsafe fn find_name(el: AXRef, depth: usize) -> Option<String> {
        if depth > 8 { return None; }
        if ax_str(el, "AXRole").as_deref() == Some("AXStaticText") {
            if let Some(v) = ax_str(el, "AXValue") { if !v.is_empty() { return Some(v); } }
        }
        let (arr, kids) = ax_kids(el);
        let mut r = None;
        for kid in kids { if r.is_none() { r = find_name(kid, depth + 1); } }
        if !arr.is_null() { CFRelease(arr as CFTypeRef); }
        r
    }
    unsafe fn walk(el: AXRef, depth: usize, out: &mut Vec<Tile>) {
        if depth >= 40 { return; }
        if ax_str(el, "AXRole").as_deref() == Some("AXMenuItem") {
            let name = find_name(el, 0).or_else(|| {
                let raw = ax_str(el, "AXTitle").filter(|s| !s.is_empty())
                    .or_else(|| ax_str(el, "AXDescription")).unwrap_or_default();
                let n = strip_ax_state(&raw);
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
        for kid in kids { walk(kid, depth + 1, out); }
        if !arr.is_null() { CFRelease(arr as CFTypeRef); }
    }

    let mut tiles = Vec::new();
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if !app.is_null() {
            use core_foundation_sys::number::kCFBooleanTrue;
            let attr = CFString::new("AXEnhancedUserInterface");
            AXUIElementSetAttributeValue(app, attr.as_concrete_TypeRef(), kCFBooleanTrue as CFTypeRef);
            walk(app, 0, &mut tiles);
            CFRelease(app as CFTypeRef);
        }
    }
    tiles
}

fn is_chromatic(r: u8, g: u8, b: u8) -> bool {
    let (r,g,b) = (r as i32, g as i32, b as i32);
    r.max(g).max(b) - r.min(g).min(b) > 35 && r.max(g).max(b) > 40
}

/// Find the best capturable Teams meeting window (non-Calendar, non-Chat).
fn find_best_window(owner: &str) -> Option<(u32, f64, f64, f64, f64)> {
    use core_graphics::window::{CGWindowListCopyWindowInfo, kCGNullWindowID, kCGWindowListOptionAll};
    let skip = ["Calendar","Chat","Activity","Calls","OneDrive","Teams NRC","Microsoft Teams\0","Viva","Engage","Copilot"];
    let list = unsafe { CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID) };
    if list.is_null() { return None; }
    let n = unsafe { CFArrayGetCount(list as _) };
    let mut meeting: Option<(u32,f64,f64,f64,f64)> = None;
    let mut fallback: Option<(f64,u32,f64,f64,f64,f64)> = None;
    for i in 0..n {
        let item = unsafe { CFArrayGetValueAtIndex(list as _, i) } as CFDictionaryRef;
        if item.is_null() { continue; }
        if cg_i32(item,"kCGWindowLayer") != 0 { continue; }
        let wo = cg_str(item,"kCGWindowOwnerName").unwrap_or_default();
        if !wo.to_lowercase().contains(&owner.to_lowercase()) { continue; }
        let id = cg_i32(item,"kCGWindowNumber") as u32; if id==0 {continue;}
        let (x,y,w,h) = cg_bounds(item);
        if w<200.0||h<200.0 {continue;}
        let title = cg_str(item,"kCGWindowName").unwrap_or_default();
        let is_meeting = !title.is_empty() && !skip.iter().any(|s|title.starts_with(s)) && title!="Microsoft Teams";
        if let Some(img) = capture_window_cg(id) {
            unsafe { CGImageRelease(img); }
            if is_meeting && meeting.is_none() { meeting = Some((id,x,y,w,h)); }
            let area = w*h;
            if fallback.as_ref().map_or(true,|(a,_,_,_,_,_)|area>*a) {
                fallback = Some((area,id,x,y,w,h));
            }
        }
    }
    unsafe { CFRelease(list as CFTypeRef); }
    meeting.or_else(||fallback.map(|(_,id,x,y,w,h)|(id,x,y,w,h)))
}

fn cg_bounds(d: CFDictionaryRef) -> (f64,f64,f64,f64) {
    let cf = CFString::new("kCGWindowBounds");
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return (0.,0.,0.,0.); }
    let bd = v as CFDictionaryRef;
    (cg_f64(bd,"X"),cg_f64(bd,"Y"),cg_f64(bd,"Width"),cg_f64(bd,"Height"))
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
    if v.is_null() { return 0.; }
    let mut n = 0f64;
    unsafe { CFNumberGetValue(v as CFNumberRef, kCFNumberFloat64Type, &mut n as *mut _ as *mut c_void); }
    n
}
fn cg_str(d: CFDictionaryRef, k: &str) -> Option<String> {
    let cf = CFString::new(k);
    let v = unsafe { CFDictionaryGetValue(d, cf.as_concrete_TypeRef() as *const c_void) };
    if v.is_null() { return None; }
    let mut buf = vec![0i8;1024];
    let ok = unsafe { CFStringGetCString(v as CFStringRef, buf.as_mut_ptr(), 1024, kCFStringEncodingUTF8) };
    if ok!=0 { unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().ok().map(|s|s.to_owned()) } } else { None }
}

/// Run one probe cycle: detect currently speaking participants.
/// Returns speaker names (empty = nobody detected = "Me" speaking if audio present).
fn probe_speakers(app: &str, pid: u32, prev_frame: &mut Vec<u8>) -> Vec<String> {
    let owner = cg_window_owner(app);
    let Some((win_id, wx, wy, ww, wh)) = find_best_window(&owner) else { return vec![]; };
    let Some(img_ref) = capture_window_cg(win_id) else { return vec![]; };
    let rgba = cg_image_to_rgba(img_ref);
    unsafe { CGImageRelease(img_ref); }
    let Some((pixels, img_w, img_h)) = rgba else { return vec![]; };

    let tiles = std::panic::catch_unwind(|| ax_find_tiles(pid as i32)).unwrap_or_default();
    if tiles.is_empty() { *prev_frame = pixels; return vec![]; }

    // Frame diff: count pixels that changed AND became chromatic
    let diff_px: u32 = if prev_frame.len() == pixels.len() {
        pixels.chunks(4).zip(prev_frame.chunks(4)).map(|(c,o)| {
            let delta = (c[0] as i32-o[0] as i32).unsigned_abs()
                      + (c[1] as i32-o[1] as i32).unsigned_abs()
                      + (c[2] as i32-o[2] as i32).unsigned_abs();
            if delta > 20 && is_chromatic(c[0],c[1],c[2]) { 1 } else { 0 }
        }).sum()
    } else { 0 };
    *prev_frame = pixels.clone();

    let sx = img_w as f64 / ww;
    let sy = img_h as f64 / wh;
    let mut speakers = Vec::new();

    for tile in &tiles {
        // Only count tiles whose center falls within the captured window
        let cx = tile.x + tile.w / 2.0;
        let cy = tile.y + tile.h / 2.0;
        if cx < wx || cx >= wx+ww || cy < wy || cy >= wy+wh { continue; }

        let rx = (tile.x - wx) * sx;
        let ry = (tile.y - wy) * sy;
        let tw = tile.w * sx;
        let th = tile.h * sy;

        // Static border chromatic check (camera-on ring)
        let (x0,y0) = ((rx as i32).max(0), (ry as i32).max(0));
        let (x1,y1) = (((rx+tw) as i32).min(img_w-1), ((ry+th) as i32).min(img_h-1));
        let border_chroma: f64 = if x1>x0 && y1>y0 {
            let (mut chroma,mut n) = (0i32,0i32);
            let mut s = |x:i32,y:i32| {
                if x<0||x>=img_w||y<0||y>=img_h{return;}
                let o=((y*img_w+x)*4) as usize;
                if is_chromatic(pixels[o],pixels[o+1],pixels[o+2]){chroma+=1;}
                n+=1;
            };
            for bw in 0..8i32 {
                let mut x=x0; while x<=x1{s(x,y0+bw);s(x,y1-bw);x+=2;}
                let mut y=y0; while y<=y1{s(x0+bw,y);s(x1-bw,y);y+=2;}
            }
            if n>0{chroma as f64/n as f64} else {0.0}
        } else { 0.0 };

        // Speaking = static ring OR new chromatic pixels appeared (camera-off avatar ring)
        if border_chroma > 0.02 || diff_px > 200 {
            speakers.push(tile.name.clone());
        }
    }
    speakers
}

// ── Post-meeting processing ───────────────────────────────────────────────────

/// Saves the speaker timeline as a JSON sidecar next to the WAV files.
/// Format is self-describing so any tool can use it alongside the WAVs.
fn save_timeline(timeline: &Timeline, stem: &Path) -> PathBuf {
    let path = stem.with_extension("speakers.json");
    let events: Vec<serde_json::Value> = timeline.events.iter().map(|e| {
        serde_json::json!({
            "offset_secs": (e.offset_secs * 100.0).round() / 100.0,
            "speakers":    e.speakers,
        })
    }).collect();
    let json = serde_json::json!({
        "participants": timeline.seen.iter().collect::<Vec<_>>(),
        "events": events,
    });
    if let Ok(s) = serde_json::to_string_pretty(&json) {
        let _ = std::fs::write(&path, s);
    }
    path
}

/// Called when all three WAVs are ready.  Saves the timeline JSON, prints the
/// file summary, then either auto-transcribes (if WHISPER_URL is set) or
/// prints instructions for doing it later.
fn process_recording(
    mixed_path:  &Path,
    others_path: &Path,
    self_path:   &Path,
    timeline:    &Timeline,
    app:         &str,
) {
    let stem         = mixed_path.with_extension("");
    let timeline_path = save_timeline(timeline, &stem);
    let vtt_path     = stem.with_extension("vtt");

    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Meeting ended: {:<46}║", app.chars().take(46).collect::<String>());
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  RECORDINGS                                                  ║");
    println!("║  others (tap):  {:<45}║", shorten(others_path, 45));
    println!("║  self   (mic):  {:<45}║", shorten(self_path,   45));
    println!("║  mixed:         {:<45}║", shorten(mixed_path,  45));
    println!("║  speakers:      {:<45}║", shorten(&timeline_path, 45));
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Participants: {:<46}║",
             timeline.seen.iter().cloned().collect::<Vec<_>>().join(", ").chars().take(46).collect::<String>());
    println!("║  Timeline events recorded: {:<34}║", timeline.events.len());
    println!("╚══════════════════════════════════════════════════════════════╝");

    let api_key     = std::env::var("OPENAI_API_KEY").unwrap_or_else(|_| "none".into());
    let whisper_url = std::env::var("WHISPER_URL").ok().or_else(|| {
        if api_key != "none" {
            Some("https://api.openai.com/v1/audio/transcriptions".into())
        } else {
            None
        }
    });

    match whisper_url {
        Some(url) => {
            // Auto-transcribe with configured service
            println!("\n  🔤 Transcribing with: {url}");
            println!("     (both others + self will be sent; results merged by timestamp)");

            // Transcribe both streams separately, then merge & attribute
            let others_segs = transcribe_with(others_path, &url, &api_key, "others");
            let self_segs   = transcribe_with(self_path,   &url, &api_key, "self");

            let mut all_segs = others_segs;
            // self segments: attribute to "Me" regardless of visual timeline
            // (mic audio is definitionally the local user)
            let me_segs: Vec<Segment> = self_segs.into_iter().map(|s| Segment {
                start:   s.start,
                end:     s.end,
                text:    s.text,
                speaker: Some("Me".into()),
            }).collect();
            all_segs.extend(me_segs);
            all_segs.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());

            // Fill in speaker attribution for others-stream segments
            for seg in all_segs.iter_mut() {
                if seg.speaker.is_none() {
                    let mid = (seg.start + seg.end) / 2.0;
                    seg.speaker = Some(timeline.speaker_at(mid)
                        .unwrap_or_else(|| "Unknown".into()));
                }
            }

            if all_segs.is_empty() {
                eprintln!("  ⚠ No transcript produced — check service and API key");
            } else {
                write_vtt_segs(&all_segs, timeline, &vtt_path);
                println!("  ✓ VTT written: {}", vtt_path.display());
                print_preview(&all_segs, 6);
            }
        }
        None => {
            // No service configured — print self-contained instructions
            println!();
            println!("  📋 To transcribe later, run:");
            println!();
            println!("  # Option A — OpenAI Whisper:");
            println!("  WHISPER_URL=https://api.openai.com/v1/audio/transcriptions \\");
            println!("  OPENAI_API_KEY=sk-... \\");
            println!("  cargo run --example transcribe -- \\");
            println!("    --others  {} \\", others_path.display());
            println!("    --self    {} \\", self_path.display());
            println!("    --speakers {} \\", timeline_path.display());
            println!("    --out      {}", vtt_path.display());
            println!();
            println!("  # Option B — Local faster-whisper server:");
            println!("  WHISPER_URL=http://localhost:8080/v1/audio/transcriptions \\");
            println!("  cargo run --example transcribe -- ...");
            println!();
            println!("  # Option C — Any OpenAI-compatible endpoint (Groq, local llama.cpp, etc.)");
            println!("  WHISPER_URL=<endpoint> OPENAI_API_KEY=<key> ...");
        }
    }
    println!();
    drop(app);
}

fn shorten(p: &Path, max: usize) -> String {
    let s = p.display().to_string();
    let s = s.replace(&std::env::var("HOME").unwrap_or_default(), "~");
    if s.len() > max { format!("…{}", &s[s.len()-max+1..]) } else { s }
}

fn print_preview(segs: &[Segment], n: usize) {
    println!("\n  ── Transcript preview ──");
    for seg in segs.iter().take(n) {
        let spk = seg.speaker.as_deref().unwrap_or("?");
        println!("  [{:>6.1}s] <{}> {}", seg.start, spk, seg.text);
    }
    if segs.len() > n { println!("  … ({} more segments)", segs.len() - n); }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let state: Arc<Mutex<Option<SessionState>>> = Arc::new(Mutex::new(None));
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let listener   = MeetingListener::new();
    let listener2  = listener.clone();

    let state_ev   = Arc::clone(&state);
    let state_rdy  = Arc::clone(&state);
    let stop_ev    = Arc::clone(&stop_flag);

    listener.on(move |event| {
        match event {
            Event::MeetingDetected { app, pid } => {
                println!("📞 Meeting detected: {app} (pid={pid})");
                let mut g = state_ev.lock().unwrap();
                *g = Some(SessionState {
                    app: app.clone(),
                    pid: *pid,
                    timeline: Timeline {
                        started_at: Some(Instant::now()),
                        ..Default::default()
                    },
                    running: true,
                });
                drop(g);

                // Start recording (tap + mic → mixed WAV)
                listener2.record();
                println!("  🔴 Recording started");
                stop_ev.store(false, std::sync::atomic::Ordering::Relaxed);
            }

            Event::RecordingReady { mixed_path, others_path, self_path, app } => {
                let g = state_rdy.lock().unwrap();
                if let Some(session) = g.as_ref() {
                    process_recording(mixed_path, others_path, self_path, &session.timeline, app);
                }
            }

            Event::MeetingEnded { app } => {
                println!("📴 Meeting ended: {app}");
                if let Some(s) = state_ev.lock().unwrap().as_mut() {
                    s.running = false;
                }
                stop_ev.store(true, std::sync::atomic::Ordering::Relaxed);
            }

            Event::RecordingStarted { app } => println!("  ▶ RecordingStarted: {app}"),
            Event::RecordingEnded   { app } => println!("  ■ RecordingEnded: {app}"),
            _ => {}
        }
    });

    listener.start().expect("MeetingListener failed to start");

    // Resolve the whisper URL the same way process_recording does
    let resolved_whisper = {
        let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        std::env::var("WHISPER_URL").ok()
            .or_else(|| if !key.is_empty() {
                Some("https://api.openai.com/v1/audio/transcriptions".into())
            } else { None })
            .unwrap_or_else(|| "(not configured — set OPENAI_API_KEY or WHISPER_URL)".into())
    };

    println!("🎙  meeting_recorder listening…");
    println!("    Outputs → {}", output_dir().display());
    println!("    Whisper → {resolved_whisper}");
    println!("    Ctrl-C to quit\n");

    // Probe loop — 100ms ticks, runs only while in a meeting
    let mut prev_frame: Vec<u8> = Vec::new();
    let mut last_speakers: BTreeSet<String> = BTreeSet::new();

    loop {
        if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        let (app, pid, running) = {
            let g = state.lock().unwrap();
            match g.as_ref() {
                Some(s) => (s.app.clone(), s.pid, s.running),
                None    => { std::thread::sleep(Duration::from_millis(200)); continue; }
            }
        };
        if !running { std::thread::sleep(Duration::from_millis(200)); continue; }

        let speakers = probe_speakers(&app, pid, &mut prev_frame);
        let cur: BTreeSet<String> = speakers.iter().cloned().collect();

        // Push to timeline on every tick (so we have dense samples for attribution)
        {
            let mut g = state.lock().unwrap();
            if let Some(s) = g.as_mut() {
                s.timeline.push(speakers.clone());
            }
        }

        // Print speaker changes to stdout
        if cur != last_speakers {
            let now = chrono::Local::now().format("%H:%M:%S.%3f");
            if cur.is_empty() {
                println!("🔇 [{now}] silence\n");
            } else {
                let names = cur.iter().cloned().collect::<Vec<_>>().join("  +  ");
                println!("🎤 [{now}] {names}\n");
            }
            last_speakers = cur;
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn output_dir() -> PathBuf {
    std::env::var("MEETINGS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs_next_home()
                .map(|h| h.join("Documents/meetings"))
                .unwrap_or_else(|| PathBuf::from("/tmp/meetings"))
        })
}

fn dirs_next_home() -> Option<PathBuf> {
    // Simple home-dir lookup without a dependency
    std::env::var("HOME").ok().map(PathBuf::from)
}
