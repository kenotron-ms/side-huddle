    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::{CFArrayRef, CFArrayGetCount, CFArrayGetValueAtIndex};
    use core_foundation_sys::base::{CFTypeRef, CFRelease};
    use core_foundation_sys::dictionary::{CFDictionaryRef, CFDictionaryGetValue};
    use core_foundation_sys::string::{
        CFStringRef, CFStringGetCString, CFStringGetLength, kCFStringEncodingUTF8,
    };
    use core_foundation_sys::number::{CFNumberRef, CFNumberGetValue, kCFNumberSInt32Type, kCFNumberFloat64Type};
    use core_graphics::window::{kCGWindowListOptionAll, kCGNullWindowID, CGWindowListCopyWindowInfo};
    use coreaudio_sys::{
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
        AudioObjectID, AudioObjectPropertyAddress,
        kAudioHardwarePropertyProcessObjectList,
        kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectSystemObject,
        kAudioProcessPropertyIsRunningInput,
        kAudioProcessPropertyPID,
    };
    use std::ffi::c_void;

    /// Returns the title of the Teams/Zoom/etc window whose PID has an active
    /// CoreAudio input session. Returns None if no such window exists.
    pub(crate) fn window_title_for_audio_input_pid(owner_substr: &str) -> Option<String> {
        let array_ref: CFArrayRef = unsafe {
            CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID) as CFArrayRef
        };
        if array_ref.is_null() { return None; }

        let audio_pids = get_active_input_pids();
        let count = unsafe { CFArrayGetCount(array_ref) };
        let owner_lower = owner_substr.to_lowercase();

        let result = (0..count).find_map(|i| {
            let item = unsafe { CFArrayGetValueAtIndex(array_ref, i) };
            if item.is_null() { return None; }
            let dict = item as CFDictionaryRef;

            // Layer 0 only (normal on-screen windows, not overlays/menus)
            if let Some(layer) = dict_get_i32(dict, "kCGWindowLayer") {
                if layer != 0 { return None; }
            }

            let owner_name = dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
            if !owner_name.to_lowercase().contains(&owner_lower) { return None; }

            let pid = dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(0) as u32;
            if pid == 0 || !audio_pids.contains(&pid) { return None; }

            dict_get_string(dict, "kCGWindowName").filter(|t| !t.is_empty())
        });

        // CGWindowListCopyWindowInfo returns a Create-rule ref — must release
        unsafe { CFRelease(array_ref as CFTypeRef); }
        result
    }

    fn get_active_input_pids() -> Vec<u32> {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut size: u32 = 0;
        unsafe {
            if AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &addr, 0, std::ptr::null(), &mut size) != 0 {
                return vec![];
            }
        }
        let count = size as usize / std::mem::size_of::<AudioObjectID>();
        let mut objs = vec![0u32; count];
        unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject, &addr, 0, std::ptr::null(),
                &mut size, objs.as_mut_ptr() as *mut c_void,
            );
        }
        objs.into_iter().filter(|&obj| {
            let ia = AudioObjectPropertyAddress {
                mSelector: kAudioProcessPropertyIsRunningInput,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            let mut r: u32 = 0;
            let mut s = std::mem::size_of::<u32>() as u32;
            unsafe {
                AudioObjectGetPropertyData(obj, &ia, 0, std::ptr::null(), &mut s, &mut r as *mut _ as *mut c_void);
            }
            r != 0
        }).map(|obj| {
            let pa = AudioObjectPropertyAddress {
                mSelector: kAudioProcessPropertyPID,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            let mut pid: i32 = 0;
            let mut s = std::mem::size_of::<i32>() as u32;
            unsafe {
                AudioObjectGetPropertyData(obj, &pa, 0, std::ptr::null(), &mut s, &mut pid as *mut _ as *mut c_void);
            }
            pid as u32
        }).collect()
    }

    /// Look up a string value from a CGWindowInfo dictionary using a raw sys key.
    fn dict_get_string(dict: CFDictionaryRef, key: &str) -> Option<String> {
        let cf_key = CFString::new(key);
        let val = unsafe {
            CFDictionaryGetValue(dict, cf_key.as_concrete_TypeRef() as *const c_void)
        };
        if val.is_null() { return None; }
        cfstring_to_string(val as CFStringRef)
    }

    /// Look up an i32 value from a CGWindowInfo dictionary.
    fn dict_get_i32(dict: CFDictionaryRef, key: &str) -> Option<i32> {
        let cf_key = CFString::new(key);
        let val = unsafe {
            CFDictionaryGetValue(dict, cf_key.as_concrete_TypeRef() as *const c_void)
        };
        if val.is_null() { return None; }
        let mut n: i32 = 0;
        let ok = unsafe {
            CFNumberGetValue(val as CFNumberRef, kCFNumberSInt32Type, &mut n as *mut _ as *mut c_void)
        };
        if ok { Some(n) } else { None }
    }

    /// Convert a CFStringRef to a Rust String via UTF-8 encoding.
    /// Allocates `len * 4 + 1` bytes (upper bound for UTF-8 from UTF-16 code units).
    fn cfstring_to_string(s: CFStringRef) -> Option<String> {
        if s.is_null() { return None; }
        unsafe {
            let len = CFStringGetLength(s);
            if len == 0 { return Some(String::new()); }
            // UTF-8 is at most 4 bytes per UTF-16 code unit
            let max_size = (len as usize) * 4 + 1;
            let mut buf = vec![0i8; max_size];
            let ok = CFStringGetCString(s, buf.as_mut_ptr(), max_size as isize, kCFStringEncodingUTF8);
            if ok != 0 {
                let cstr = std::ffi::CStr::from_ptr(buf.as_ptr());
                cstr.to_str().ok().map(|s| s.to_owned())
            } else {
                None
            }
        }
    }

    // ── Window watcher helpers ────────────────────────────────────────────────
    //
    // These mirror the Go window_darwin.go findPrimaryWindow / cgWindowExists /
    // cgWindowOwner functions.  They are used by WindowWatcher (window_watcher.rs)
    // to identify the call window when a meeting starts and then watch for its
    // closure.

    /// kCGWindowListOptionOnScreenOnly  (CGWindowListOption bitmask bit 0)
    const CG_ON_SCREEN_ONLY: u32 = 1;
    /// kCGWindowListExcludeDesktopElements  (CGWindowListOption bitmask bit 4)
    const CG_EXCL_DESKTOP: u32 = 1 << 4;

    /// Find the most prominent on-screen layer-0 window whose owner name contains
    /// `owner_substr` (case-insensitive).  Returns `(CGWindowID, title)`.
    ///
    /// "Most prominent" = largest area, which reliably selects the real call /
    /// main window over the many tiny hidden auxiliary windows (1×1 NRC stubs,
    /// toolbar strips, etc.) that apps such as Teams keep around permanently.
    ///
    /// We deliberately do NOT check per-window audio-input state here.  Teams 2.x
    /// routes audio through worker-helper processes that own no CGWindowList
    /// windows, so a PID-level audio check always returns false during a call.
    /// The caller (WindowWatcher) already knows a meeting is active because
    /// `fire_meeting_started` ran; we just need the right window to watch.
    pub(crate) fn find_primary_window(owner_substr: &str) -> Option<(u32, String)> {
        let array_ref: CFArrayRef = unsafe {
            CGWindowListCopyWindowInfo(
                (CG_ON_SCREEN_ONLY | CG_EXCL_DESKTOP) as _,
                kCGNullWindowID,
            ) as CFArrayRef
        };
        if array_ref.is_null() { return None; }

        let count = unsafe { CFArrayGetCount(array_ref) };
        let owner_lower = owner_substr.to_lowercase();

        let mut best_id:    Option<u32> = None;
        let mut best_area:  f64         = 0.0;
        let mut best_title: String      = String::new();

        for i in 0..count {
            let item = unsafe { CFArrayGetValueAtIndex(array_ref, i) };
            if item.is_null() { continue; }
            let dict = item as CFDictionaryRef;

            // Layer 0 only (normal application windows, not overlays or HUDs)
            if let Some(layer) = dict_get_i32(dict, "kCGWindowLayer") {
                if layer != 0 { continue; }
            }

            // Owner name must contain our app substring
            let owner = dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
            if !owner.to_lowercase().contains(&owner_lower) { continue; }

            // Skip tiny background windows (< 100×100 = 10 000 px²)
            let area = window_area(dict);
            if area < 10_000.0 { continue; }

            if area > best_area {
                let win_id = dict_get_i32(dict, "kCGWindowNumber").unwrap_or(0) as u32;
                if win_id == 0 { continue; }
                best_area  = area;
                best_id    = Some(win_id);
                best_title = dict_get_string(dict, "kCGWindowName")
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "(active window)".to_string());
            }
        }

        unsafe { CFRelease(array_ref as CFTypeRef); }
        best_id.map(|id| (id, best_title))
    }

    /// Returns `true` if a window with `window_id` is still present in the full
    /// window list (including hidden windows).
    pub(crate) fn window_exists(window_id: u32) -> bool {
        let array_ref: CFArrayRef = unsafe {
            CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID) as CFArrayRef
        };
        if array_ref.is_null() { return false; }

        let count = unsafe { CFArrayGetCount(array_ref) };
        let found = (0..count).any(|i| {
            let item = unsafe { CFArrayGetValueAtIndex(array_ref, i) };
            if item.is_null() { return false; }
            let dict = item as CFDictionaryRef;
            dict_get_i32(dict, "kCGWindowNumber")
                .map_or(false, |id| id as u32 == window_id)
        });

        unsafe { CFRelease(array_ref as CFTypeRef); }
        found
    }

    /// Converts a friendly meeting-app name to the CGWindowOwnerName substring
    /// used to match its windows in `CGWindowListCopyWindowInfo`.
    ///
    /// Mirrors Go's `cgWindowOwner()` in window_darwin.go exactly.
    pub(crate) fn cg_window_owner(app: &str) -> String {
        let lower = app.to_lowercase();
        if lower.contains("teams")                              { return "Microsoft Teams".into(); }
        if lower.contains("zoom")                               { return "zoom.us".into(); }
        if lower.contains("webex")                              { return "Webex".into(); }
        if lower.contains("slack")                              { return "Slack".into(); }
        if lower.contains("google meet") || lower.contains("chrome") { return "Google Chrome".into(); }
        if lower.contains("safari")                             { return "Safari".into(); }
        if lower.contains("firefox")                            { return "Firefox".into(); }
        app.to_string()
    }

    /// Read the area (width × height) of the window from its `kCGWindowBounds`
    /// sub-dictionary.  Returns 0.0 if the key is absent or the values are 0.
    fn window_area(dict: CFDictionaryRef) -> f64 {
        let cf_key = CFString::new("kCGWindowBounds");
        let val = unsafe {
            CFDictionaryGetValue(dict, cf_key.as_concrete_TypeRef() as *const c_void)
        };
        if val.is_null() { return 0.0; }
        let sub = val as CFDictionaryRef;
        let w = dict_get_f64(sub, "Width").unwrap_or(0.0);
        let h = dict_get_f64(sub, "Height").unwrap_or(0.0);
        w * h
    }

    /// Look up an f64 value from a CFDictionary using a string key.
    fn dict_get_f64(dict: CFDictionaryRef, key: &str) -> Option<f64> {
        let cf_key = CFString::new(key);
        let val = unsafe {
            CFDictionaryGetValue(dict, cf_key.as_concrete_TypeRef() as *const c_void)
        };
        if val.is_null() { return None; }
        let mut n: f64 = 0.0;
        let ok = unsafe {
            CFNumberGetValue(
                val as CFNumberRef,
                kCFNumberFloat64Type,
                &mut n as *mut _ as *mut c_void,
            )
        };
        if ok { Some(n) } else { None }
    }
