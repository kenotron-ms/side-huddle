    use coreaudio_sys::{
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
        AudioObjectID, AudioObjectPropertyAddress,
        kAudioHardwarePropertyProcessObjectList,
        kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectSystemObject,
        kAudioProcessPropertyBundleID,
        kAudioProcessPropertyIsRunningInput,
        kAudioProcessPropertyPID,
    };
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation::base::TCFType;
    use std::ffi::c_void;

    use crate::apps::{identify_by_bundle, is_browser_bundle};
    use super::process::proc_name_for_pid;
    use super::window::{cg_window_owner, find_primary_window, window_title_for_audio_input_pid};

    /// Poll CoreAudio process objects. Returns (pid, friendly_app_name) of the
    /// first meeting-app process that currently has IsRunningInput == 1.
    pub(crate) fn poll_active() -> (u32, String) {
        let objs = get_process_objects();
        for &obj in &objs {
            let Some(bundle) = get_bundle_id(obj) else { continue };
            if !is_meeting_bundle(&bundle) { continue; }
            if !is_running_input(obj) { continue; }

            let pid = get_pid(obj);

            // For browsers, cross-check the window title
            if is_browser_bundle(&bundle) {
                let owner = bundle_to_owner_name(&bundle);
                if let Some(title) = window_title_for_audio_input_pid(&owner) {
                    if let Some(app) = crate::apps::identify_by_window_title(&title) {
                        return (pid, app.to_string());
                    }
                }
                continue; // browser but no meeting window
            }

            let app = identify_by_bundle(&bundle)
                .map(|s| s.to_string())
                .or_else(|| {
                    let name = proc_name_for_pid(pid);
                    crate::apps::identify_by_proc_name(&name).map(|s| s.to_string())
                })
                .unwrap_or_else(|| bundle.clone());

            // For native apps, apply a window-title pre-join guard — the same in
            // spirit as the browser window-title gate above. Meeting apps such as
            // Zoom activate the microphone during their pre-join screen (camera /
            // mic level preview), which makes `IsRunningInput` fire before the user
            // is actually in a call. If the primary window title matches a known
            // pre-join pattern, skip this poll cycle and wait for a real meeting.
            //
            // Limitation: Teams pre-join titles are indistinguishable from
            // in-meeting titles by window name; that case is not filtered here.
            let owner = cg_window_owner(&app);
            if let Some((_id, ref title)) = find_primary_window(&owner) {
                if crate::apps::is_prejoin_window_title(title) {
                    continue; // clear pre-join state — not in a meeting yet
                }
            }

            return (pid, app);
        }
        (0, String::new())
    }

    fn is_meeting_bundle(bundle: &str) -> bool {
        crate::apps::MEETING_BUNDLES.iter().any(|b| bundle.contains(b))
    }

    fn bundle_to_owner_name(bundle: &str) -> String {
        let b = bundle.to_lowercase();
        if b.contains("com.google.chrome") { return "Google Chrome".into(); }
        if b.contains("com.apple.safari")  { return "Safari".into(); }
        if b.contains("org.mozilla")       { return "Firefox".into(); }
        if b.contains("com.microsoft.edge"){ return "Microsoft Edge".into(); }
        bundle.to_string()
    }

    // ── CoreAudio helpers ─────────────────────────────────────────────────────────

    fn get_process_objects() -> Vec<AudioObjectID> {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut size: u32 = 0;
        unsafe {
            if AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &addr, 0, std::ptr::null(), &mut size) != 0
                || size == 0 { return Vec::new(); }
        }
        let count = size as usize / std::mem::size_of::<AudioObjectID>();
        let mut objs = vec![0u32; count];
        unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject, &addr, 0, std::ptr::null(),
                &mut size, objs.as_mut_ptr() as *mut c_void,
            );
        }
        objs
    }

    fn get_pid(obj: AudioObjectID) -> u32 {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioProcessPropertyPID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut pid: i32 = 0;
        let mut size = std::mem::size_of::<i32>() as u32;
        unsafe {
            AudioObjectGetPropertyData(obj, &addr, 0, std::ptr::null(), &mut size, &mut pid as *mut _ as *mut c_void);
        }
        pid as u32
    }

    fn is_running_input(obj: AudioObjectID) -> bool {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioProcessPropertyIsRunningInput,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut val: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        unsafe {
            AudioObjectGetPropertyData(obj, &addr, 0, std::ptr::null(), &mut size, &mut val as *mut _ as *mut c_void);
        }
        val != 0
    }

    fn get_bundle_id(obj: AudioObjectID) -> Option<String> {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioProcessPropertyBundleID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut cf_ref: CFStringRef = std::ptr::null();
        let mut size = std::mem::size_of::<CFStringRef>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                obj, &addr, 0, std::ptr::null(),
                &mut size, &mut cf_ref as *mut _ as *mut c_void,
            )
        };
        if status != 0 || cf_ref.is_null() { return None; }
        let cf_string = unsafe { CFString::wrap_under_create_rule(cf_ref) };
        Some(cf_string.to_string())
    }
    