/// Rust demo — shows the full event lifecycle.
///
/// Usage:
///   cargo run --example demo
///   OPENAI_API_KEY=sk-... cargo run --example demo

use side_huddle::{Event, MeetingListener, PermissionGranted};
use std::io::Write as _;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("side-huddle — waiting for Teams / Zoom / Google Meet…\n");

    let listener = MeetingListener::new();

    // ── Handler 1: full lifecycle log ──────────────────────────────────────
    listener.on(|event| match event {
        Event::PermissionStatus { permission, status } => {
            let icon = match status {
                PermissionGranted::Granted      => "✅",
                PermissionGranted::NotRequested => "⏳",
                PermissionGranted::Denied       => "❌",
            };
            println!("{icon} permission: {permission:?} → {status:?}");
        }
        Event::PermissionsGranted               => println!("✅ all permissions granted"),
        Event::MeetingDetected  { app }         => println!("🟢  detected:  {app}"),
        Event::MeetingUpdated   { app, title }  => println!("📋  updated:   {app} — \"{title}\""),
        Event::RecordingStarted { app }         => println!("⏺   recording: {app} started"),
        Event::MeetingEnded     { app }         => println!("🔴  ended:     {app}"),
        Event::RecordingEnded   { app }         => println!("⏹   recording: {app} stopped"),
        Event::RecordingReady   { path, app }   => println!("💾  saved:     {app} → {}", path.display()),
        Event::CaptureStatus    { kind, capturing } =>
            println!("📡  capture:   {kind:?} capturing={capturing}"),
        Event::Error            { message }     => eprintln!("⚠️   error:     {message}"),
    });

    // ── Handler 2: prompt user before recording ────────────────────────────
    let l = listener.clone();
    listener.on(move |event| {
        if let Event::MeetingDetected { app } = event {
            print!("   Record {app}? [y/N] ");
            let _ = std::io::stdout().flush();
            let mut buf = String::new();
            if std::io::stdin().read_line(&mut buf).is_ok()
                && buf.trim().eq_ignore_ascii_case("y")
            {
                l.record();
            }
        }
    });

    // ── Handler 3: transcribe when WAV is ready ────────────────────────────
    listener.on(|event| {
        if let Event::RecordingReady { path, .. } = event {
            if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
                println!("📝  transcribing…");
                match transcribe_wav(path, &api_key) {
                    Ok(text) => {
                        let txt = path.with_extension("txt");
                        if let Ok(mut f) = std::fs::File::create(&txt) {
                            let _ = f.write_all(text.as_bytes());
                        }
                        println!("✅  transcript → {}\n---\n{text}\n---", txt.display());
                    }
                    Err(e) => eprintln!("   transcription failed: {e}"),
                }
            }
        }
    });

    listener.start()?;

    ctrlc::set_handler(|| { println!("\nshutting down…"); std::process::exit(0); })?;
    loop { std::thread::sleep(std::time::Duration::from_secs(1)); }
}

fn transcribe_wav(path: &Path, api_key: &str) -> Result<String, Box<dyn std::error::Error>> {
    let wav_bytes = std::fs::read(path)?;
    let boundary  = "----side_huddle_boundary";
    let mut body  = Vec::new();

    write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-1\r\n")?;
    write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"temperature\"\r\n\r\n0\r\n")?;
    write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n")?;
    body.extend_from_slice(&wav_bytes);
    write!(body, "\r\n--{boundary}--\r\n")?;

    let response = ureq::post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send(&body)?;

    let text = response.into_body().read_to_string()?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    Ok(json["text"].as_str().unwrap_or("").to_string())
}
