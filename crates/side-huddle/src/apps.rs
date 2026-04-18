    /// Known meeting application bundle IDs (macOS) and process names (all platforms).

    /// macOS CoreAudio bundle ID substrings that indicate a meeting app.
    pub(crate) static MEETING_BUNDLES: &[&str] = &[
        "com.microsoft.teams2",
        "com.microsoft.teams",
        "us.zoom.xos",
        "com.cisco.webexmeetingsapp",
        "com.apple.FaceTime",
        "com.tinyspeck.slackmacgap",
        "com.hnc.Discord",
        // Browsers — cross-referenced with window titles for web-based meetings
        "com.google.chrome",
        "com.apple.Safari",
        "org.mozilla.firefox",
        "com.microsoft.edgemac",
    ];

    /// Maps a bundle ID or process name substring to a friendly display name.
    pub(crate) fn identify_by_bundle(bundle: &str) -> Option<&'static str> {
        let b = bundle.to_lowercase();
        if b.contains("com.microsoft.teams") { return Some("Microsoft Teams"); }
        if b.contains("us.zoom.xos")         { return Some("Zoom"); }
        if b.contains("cisco.webex")         { return Some("Cisco Webex"); }
        if b.contains("facetime")            { return Some("FaceTime"); }
        if b.contains("slackmacgap")         { return Some("Slack"); }
        if b.contains("discord")             { return Some("Discord"); }
        // Browsers: return None — caller must cross-check window titles
        None
    }

    /// Maps a process name (from proc_name / /proc/*/comm) to a friendly name.
    pub(crate) fn identify_by_proc_name(name: &str) -> Option<&'static str> {
        let n = name.to_lowercase();
        if n == "msteams" || n.contains("teams")            { return Some("Microsoft Teams"); }
        if n == "zoom.us" || n == "zoom"                    { return Some("Zoom"); }
        if n.contains("webex")                              { return Some("Cisco Webex"); }
        if n == "facetime"                                  { return Some("FaceTime"); }
        if n == "slack"                                     { return Some("Slack"); }
        if n == "discord"                                   { return Some("Discord"); }
        None
    }

    /// Window title patterns that indicate a browser-based meeting.
    pub(crate) fn identify_by_window_title(title: &str) -> Option<&'static str> {
        if title.contains("Google Meet") || title.contains("meet.google.com") {
            return Some("Google Meet");
        }
        None
    }

    /// Bundle IDs that are browsers (require window title check).
    pub(crate) fn is_browser_bundle(bundle: &str) -> bool {
        let b = bundle.to_lowercase();
        b.contains("com.google.chrome")
            || b.contains("com.apple.safari")
            || b.contains("org.mozilla.firefox")
            || b.contains("com.microsoft.edgemac")
    }
    