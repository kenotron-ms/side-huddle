// Command demo — side-huddle Go demo (CGo-backed).
//
// Usage:
//
//	make run-demo
//
// Local transcription via whisper.cpp (`brew install whisper-cpp` + download
// a model) automatically engages after each recording if `whisper-cli` is on
// PATH and a model is at ~/.local/share/whisper/models/ggml-small.en.bin
// (override via WHISPER_MODEL env).
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"os/user"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"syscall"
	"time"

	sh "github.com/kenotron-ms/side-huddle/bindings/go"
)

// ── Speaker timeline ──────────────────────────────────────────────────────────

type speakerEntry struct {
	at       time.Time
	speakers []string // empty = silence
}

// ── Main ──────────────────────────────────────────────────────────────────────

func main() {
	// macOS only: bring up NSApplication on the pinned main OS thread so the
	// Cocoa run loop pumps UI events. Without this, permission dialogs lose
	// focus, the Dock icon bounces forever, and ⌘Q does nothing. The actual
	// listener work runs on a background goroutine.
	cocoaActivate()
	go runListener()
	cocoaRun() // blocks on [NSApp run] until runListener calls cocoaTerminate
}

func runListener() {
	// When runListener returns, tell NSApp to exit so the process terminates.
	defer cocoaTerminate()

	fmt.Printf("SideHuddle %s — waiting for Teams / Zoom / Google Meet…\n\n", sh.Version())

	// Surface permission dialogs at launch so the user can grant access before
	// the first meeting is detected.
	//
	// Both calls now dispatch to the main queue and temporarily switch to
	// NSApplicationActivationPolicyRegular (see perms_darwin.m), which ensures
	// the dialogs are attributed to SideHuddle and can hold focus — previously
	// CGRequestScreenCaptureAccess self-dismissed in Accessory mode on macOS 26.
	sh.RequestMicrophone()
	sh.RequestScreenCapture()
	shOverlayWarmup() // pre-create panel so first show is instant

	listener := sh.New()

	// Recordings go under ~/Documents/SideHuddle; each meeting gets its own
	// subfolder (created after RecordingReady — see organizeRecording).
	baseDir := mustOutputBaseDir()
	listener.SetOutputDir(baseDir)

	var timeline []speakerEntry
	var recordingStarted time.Time
	var meeting meetingState
	var titleStop chan struct{} // closed when current recording ends

	listener.On(func(e *sh.Event) {
		switch e.Kind {
		case sh.PermissionStatus:
			icon := map[sh.PermStatus]string{
				sh.Granted:      "✅",
				sh.NotRequested: "⏳",
				sh.Denied:       "❌",
			}[e.PermStatus]
			fmt.Printf("%s  %v: %v\n", icon, e.Permission, e.PermStatus)

		case sh.PermissionsGranted:
			fmt.Println("✅  permissions OK")

		case sh.MeetingDetected:
			fmt.Printf("🟢  meeting detected: %s\n", e.App)
			app := e.App
			meeting = meetingState{
				started:          time.Now(),
				app:              app,
				participantsSeen: make(map[string]bool),
			}
			// Show the overlay and return immediately — blocking here would
			// stall the Rust-side CGo event dispatch and prevent MeetingEnded
			// from ever firing (recording would never stop).
			// The goroutine waits for the user's tap (or 60s timeout) then
			// starts recording.  recordingStarted is set when the user confirms
			// so SpeakerChanged offsets reflect actual recording time.
			go func() {
				overlayMeetingDetected(app)
				if !waitOverlayRecord() {
					fmt.Println("   skipping.")
					return
				}
				recordingStarted = time.Now()
				fmt.Println("   recording.")
				overlayRecording(app)
				listener.Record()
			}()

		case sh.MeetingUpdated:
			kept := filterWindowTitle(e.Title, e.App)
			if kept == "" {
				fmt.Printf("📝  title (filtered): %q\n", e.Title)
			} else {
				fmt.Printf("📝  title: %q\n", kept)
				meeting.title = kept
				// If we're already recording, refresh the menu bar with the
				// title that just became known (window watcher fires after
				// MeetingDetected + often after RecordingStarted).
				cocoaSetRecording(true, meeting.app, meeting.title)
			}

		case sh.RecordingStarted:
			fmt.Println("⏺   recording…")
			cocoaSetRecording(true, meeting.app, meeting.title)
			// Start a window-title poller: the Rust core's MeetingUpdated
			// emits only once and may grab a chrome title (Calendar tab).
			// Scan CGWindowList ourselves until a meeting-shaped title lands.
			// The poller also watches for the meeting window to close and
			// calls StopRecording() as a Go-side fallback alongside the
			// Rust window watcher.
			titleStop = make(chan struct{})
			go pollMeetingTitle(meeting.app, &meeting, listener, titleStop)

		case sh.SpeakerChanged:
			entry := speakerEntry{at: time.Now(), speakers: e.Speakers}
			timeline = append(timeline, entry)
			offset := time.Since(recordingStarted).Round(time.Millisecond)
			if len(e.Speakers) == 0 {
				fmt.Printf("   🔇 [%s] silence\n", fmtOffset(offset))
			} else {
				fmt.Printf("   🎤 [%s] %s\n", fmtOffset(offset), strings.Join(e.Speakers, " + "))
				// Track unique remote-speaker names for 1:1 detection later.
				if meeting.participantsSeen == nil {
					meeting.participantsSeen = make(map[string]bool)
				}
				for _, name := range e.Speakers {
					meeting.participantsSeen[name] = true
				}
			}

		case sh.MeetingEnded:
			fmt.Println("🔴  meeting ended")
			cocoaNotify("Meeting ended", e.App+" — saving recording")

		case sh.RecordingEnded:
			fmt.Println("⏹   saving…")
			cocoaSetRecording(false, "", "")
			if titleStop != nil {
				close(titleStop)
				titleStop = nil
			}

		case sh.RecordingReady:
			organized := organizeRecording(e, meeting, baseDir)
			cocoaSetRecording(false, "", "") // defensive — already cleared on RecordingEnded
			fmt.Printf("💾  saved:\n")
			fmt.Printf("    mixed  → %s\n", organized.Path)
			fmt.Printf("    others → %s\n", organized.OthersPath)
			fmt.Printf("    self   → %s\n\n", organized.SelfPath)
			printTimeline(timeline, recordingStarted)
			durationSec := int(time.Since(recordingStarted).Seconds())
			overlayTranscribingSaved(durationSec)
			// Snapshot state for the transcription goroutine — a new meeting
			// can start before transcription finishes, so we freeze the
			// current meeting's data here.
			capturedOrganized := organized
			capturedMeeting := meeting
			capturedTimeline := append([]speakerEntry(nil), timeline...)
			capturedRecStart := recordingStarted
			go func() {
				offerTranscription(&sh.Event{Path: capturedOrganized.Path, OthersPath: capturedOrganized.OthersPath, SelfPath: capturedOrganized.SelfPath}, capturedMeeting, capturedTimeline, capturedRecStart)
				shOverlayHide()
			}()
			timeline = timeline[:0] // reset for the next meeting

		case sh.CaptureStatus:
			if !e.Capturing {
				fmt.Printf("⚠️   capture interrupted (%v)\n", e.CaptureKind)
			}

		case sh.Error:
			fmt.Fprintf(os.Stderr, "⚠️   error: %s\n", e.Message)
		}
	})

	if err := listener.Start(); err != nil {
		fmt.Fprintln(os.Stderr, "failed to start:", err)
		os.Exit(1)
	}
	defer listener.Stop()

	quit := make(chan os.Signal, 1)
	signal.Notify(quit, os.Interrupt, syscall.SIGTERM)

	// Keep the listener alive across meetings — this is a menu-bar agent, not
	// a one-shot. RecordingReady handles save + overlay inline; we just wait
	// here for ⌘Q (→ cocoaTerminate) or SIGINT to shut down cleanly.
	select {
	case <-quit:
		fmt.Println("\nshutting down…")
	}
}

// ── Timeline display ──────────────────────────────────────────────────────────

func printTimeline(tl []speakerEntry, start time.Time) {
	if len(tl) == 0 {
		fmt.Println("(no speaker detections recorded)")
		return
	}
	fmt.Println("── Speaker timeline ─────────────────────────────────────")
	last := ""
	for _, e := range tl {
		offset := e.at.Sub(start)
		who := "silence"
		if len(e.speakers) > 0 {
			who = strings.Join(e.speakers, " + ")
		}
		if who != last {
			fmt.Printf("  [%s] %s\n", fmtOffset(offset), who)
			last = who
		}
	}
	fmt.Println("─────────────────────────────────────────────────────────")
	fmt.Println()
}

func fmtOffset(d time.Duration) string {
	s := int(d.Seconds())
	return fmt.Sprintf("%d:%02d", s/60, s%60)
}

// ── Transcription (local via whisper.cpp) ───────────────────────────────────
//
// Shells out to whisper-cli (`brew install whisper-cpp`) with a local GGML
// model — no API key, no network, no cloud. Model is found at:
//   $WHISPER_MODEL (if set), else ~/.local/share/whisper/models/ggml-small.en.bin
//
// Install model with:
//   curl -L -o ~/.local/share/whisper/models/ggml-small.en.bin \
//     https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin
//
// Env overrides:
//   WHISPER_MODEL       — path to GGML model (.en.bin variants are English-only;
//                         use a multilingual model for WHISPER_LANG to apply)
//   WHISPER_LANG        — ISO-639-1 code; defaults to "en"
//   WHISPER_VAD_MODEL   — enables Silero VAD filtering when set to a GGML
//                         VAD model (e.g. ggml-silero-v5.1.2.bin)
//   WHISPER_CONCURRENCY — parallel whisper-cli processes; defaults to 1 to
//                         avoid model-load RAM pressure. Set to 3 on machines
//                         with headroom to restore the old parallel behavior.
//   WHISPER_VERBOSE     — 1 = surface whisper-cli + Metal/ggml diagnostics

// WAV header = 44 bytes; 0.1s at 16 kHz mono = 3200 bytes of samples → min ~3244 bytes.
const minWAVBytes = 3244

type segment struct {
	Start float64
	End   float64
	Text  string
}

type transcriptResult struct {
	label    string
	path     string
	segments []segment
	err      error
}

func offerTranscription(ev *sh.Event, m meetingState, timeline []speakerEntry, recStart time.Time) {
	whisperBin := findWhisperCli()
	if whisperBin == "" {
		// LaunchServices launches the .app with PATH=/usr/bin:/bin (no
		// /opt/homebrew/bin), so a plain `whisper-cli` LookPath returns
		// "not found" even when whisper-cpp is installed via Homebrew.
		// Surface this via a notification so the user knows transcription
		// silently skipped — the previous version returned silently here.
		fmt.Println("(install whisper-cpp to enable local transcription: brew install whisper-cpp)")
		cocoaNotify("Transcription skipped", "whisper-cli not found — install with `brew install whisper-cpp`")
		return
	}
	modelPath := whisperModelPath()
	if _, err := os.Stat(modelPath); err != nil {
		fmt.Printf("(whisper model not found at %s — download a .bin from huggingface.co/ggerganov/whisper.cpp)\n", modelPath)
		cocoaNotify("Transcription skipped", fmt.Sprintf("whisper model not found at %s", modelPath))
		return
	}
	// .en.bin variants are English-only: whisper-cli silently ignores -l for
	// them. Surface a warning so users don't wonder why their WHISPER_LANG
	// setting produced English output.
	if lang := os.Getenv("WHISPER_LANG"); lang != "" && lang != "en" && strings.HasSuffix(modelPath, ".en.bin") {
		fmt.Printf("⚠️   WHISPER_LANG=%s ignored — %s is English-only. Use a multilingual model (e.g. ggml-small.bin).\n", lang, filepath.Base(modelPath))
	}

	folder := filepath.Dir(ev.Path)
	meetingLabel := filepath.Base(folder)

	// Determine display names for 1:1 diarization.
	one2one := isOneOnOne(m)
	var otherName, myName string
	if one2one {
		otherName = otherPersonName(m)
		myName = myDisplayName()
		fmt.Printf("\U0001f465  1:1 detected — %s (them) vs %s (me)\n", otherName, myName)
	}

	cocoaNotifyWithFolder("Transcribing…", meetingLabel, folder)
	fmt.Println("\U0001f4dd  transcribing all streams…")

	streams := []struct{ label, path string }{
		{"mixed", ev.Path},
		{"others", ev.OthersPath},
		{"self", ev.SelfPath},
	}
	ch := make(chan transcriptResult, len(streams))

	// Default to serial transcription so large models don't triple-load into
	// RAM (small = 3×~466MB, medium = 3×~1.5GB). Override with
	// WHISPER_CONCURRENCY=3 on machines with headroom to restore parallelism.
	sem := make(chan struct{}, whisperConcurrency())
	for _, r := range streams {
		r := r
		go func() {
			sem <- struct{}{}
			defer func() { <-sem }()
			fi, err := os.Stat(r.path)
			if err != nil || fi.Size() < minWAVBytes {
				ch <- transcriptResult{r.label, r.path, nil, nil}
				return
			}
			fmt.Printf("📝  transcribing %s…\n", r.label)
			segs, err := transcribeWAV(r.path, whisperBin)
			ch <- transcriptResult{r.label, r.path, segs, err}
		}()
	}

	results := map[string][]segment{}
	for range streams {
		r := <-ch
		if r.err != nil {
			fmt.Fprintf(os.Stderr, "\u26a0\ufe0f   transcription failed (%s): %v\n", r.label, r.err)
			continue
		}
		if len(r.segments) == 0 {
			continue
		}
		// Determine fixed speaker label for this stream (1:1 only).
		fixedSpeaker := ""
		if one2one {
			switch r.label {
			case "others":
				fixedSpeaker = otherName
			case "self":
				fixedSpeaker = myName
			}
		} else if r.label == "self" {
			fixedSpeaker = "Me"
		}
		// Write plain-text transcript alongside the WAV.
		txtPath := strings.TrimSuffix(r.path, ".wav") + ".txt"
		var sb strings.Builder
		for _, s := range r.segments {
			if fixedSpeaker != "" {
				fmt.Fprintf(&sb, "[%s] <%s> %s\n", fmtSecs(s.Start), fixedSpeaker, strings.TrimSpace(s.Text))
			} else {
				fmt.Fprintf(&sb, "[%s] %s\n", fmtSecs(s.Start), strings.TrimSpace(s.Text))
			}
		}
		_ = os.WriteFile(txtPath, []byte(sb.String()), 0644)
		fmt.Printf("\u2705  %s \u2192 %s\n", r.label, txtPath)
		results[r.label] = r.segments
	}

	// For 1:1: also write a merged transcript sorted by timestamp.
	if one2one {
		writeMerged1on1Transcript(ev, results, otherName, myName, folder)
	}

	fmt.Println()
	printed := false
	for _, label := range []string{"mixed", "others", "self"} {
		segs, ok := results[label]
		if !ok {
			continue
		}
		fmt.Printf("\u2500\u2500 Transcript (%s) %s\n", label, strings.Repeat("\u2500", max(0, 38-len(label))))
		for _, s := range segs {
			var speaker string
			switch {
			case one2one && label == "others":
				speaker = " <" + otherName + ">"
			case one2one && label == "self":
				speaker = " <" + myName + ">"
			case label == "mixed" && len(timeline) > 0:
				if sp := speakerAt(timeline, recStart, s.Start, s.End); sp != "" {
					speaker = " <" + sp + ">"
				}
			case label == "self":
				speaker = " <me>"
			}
			fmt.Printf("  [%s]%s %s\n", fmtSecs(s.Start), speaker, strings.TrimSpace(s.Text))
		}
		printed = true
	}
	if !printed {
		fmt.Println("(no transcript — audio may have been too short)")
	}
	fmt.Println(strings.Repeat("\u2500", 57))

	cocoaNotifyWithFolder("Transcript ready", meetingLabel, folder)
}

// writeMerged1on1Transcript writes a single conversation-style .txt file that
// interleaves both speakers in chronological order, named after the meeting.
func writeMerged1on1Transcript(ev *sh.Event, results map[string][]segment,
	otherName, myName, folder string) {
	others := results["others"]
	self := results["self"]
	if len(others) == 0 && len(self) == 0 {
		return
	}
	type turn struct {
		start  float64
		speaker string
		text   string
	}
	var turns []turn
	for _, s := range others {
		turns = append(turns, turn{s.Start, otherName, strings.TrimSpace(s.Text)})
	}
	for _, s := range self {
		turns = append(turns, turn{s.Start, myName, strings.TrimSpace(s.Text)})
	}
	// Sort by start time
	for i := 1; i < len(turns); i++ {
		for j := i; j > 0 && turns[j].start < turns[j-1].start; j-- {
			turns[j], turns[j-1] = turns[j-1], turns[j]
		}
	}
	var sb strings.Builder
	for _, t := range turns {
		fmt.Fprintf(&sb, "[%s] <%s> %s\n", fmtSecs(t.start), t.speaker, t.text)
	}
	mergedPath := filepath.Join(folder, "conversation.txt")
	_ = os.WriteFile(mergedPath, []byte(sb.String()), 0644)
	fmt.Printf("\u2705  1:1 merged \u2192 %s\n", mergedPath)
}

// speakerAt finds the most active speaker during [segStart, segEnd] seconds
// from the recording timeline.
func speakerAt(tl []speakerEntry, recStart time.Time, segStart, segEnd float64) string {
	mid := recStart.Add(time.Duration((segStart+segEnd)/2 * float64(time.Second)))
	window := time.Duration((segEnd-segStart)/2*float64(time.Second)) + 500*time.Millisecond

	// Count how often each name appears in the window around the midpoint
	counts := map[string]int{}
	for _, e := range tl {
		if e.at.Before(mid.Add(-window)) || e.at.After(mid.Add(window)) {
			continue
		}
		for _, name := range e.speakers {
			counts[name]++
		}
	}
	best, bestN := "", 0
	for name, n := range counts {
		if n > bestN {
			best, bestN = name, n
		}
	}
	return best
}

func fmtSecs(s float64) string {
	total := int(s)
	return fmt.Sprintf("%d:%02d", total/60, total%60)
}

// whisperModelPath resolves the GGML model file whisper-cli should load.
// Override via WHISPER_MODEL=/path/to/other-model.bin.
func whisperModelPath() string {
	if p := os.Getenv("WHISPER_MODEL"); p != "" {
		return p
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".local/share/whisper/models/ggml-small.en.bin")
}

// whisperVADModelPath resolves an optional Silero VAD model. whisper-cli's
// --vad flag needs a VAD model file; without one, we skip VAD entirely.
// Env override: WHISPER_VAD_MODEL=/path/to/ggml-silero-*.bin. Default lookup:
// ~/.local/share/whisper/models/ggml-silero-v5.1.2.bin. Returns "" when no
// file is found so callers can detect the disabled state.
func whisperVADModelPath() string {
	paths := []string{os.Getenv("WHISPER_VAD_MODEL")}
	home, _ := os.UserHomeDir()
	paths = append(paths, filepath.Join(home, ".local/share/whisper/models/ggml-silero-v5.1.2.bin"))
	for _, p := range paths {
		if p == "" {
			continue
		}
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	return ""
}

// whisperConcurrency returns how many whisper-cli processes may run in
// parallel. Default 1 (serial) to avoid model-load RAM pressure. Override
// with WHISPER_CONCURRENCY=N (N>=1).
func whisperConcurrency() int {
	if v := os.Getenv("WHISPER_CONCURRENCY"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			return n
		}
	}
	return 1
}

// findWhisperCli resolves the whisper-cli binary even when the process was
// launched by LaunchServices (Finder / `open Foo.app`), where /opt/homebrew/bin
// is not on PATH. Falls through Homebrew's standard install locations so we
// don't silently disable transcription for bundle launches.
func findWhisperCli() string {
	if p, err := exec.LookPath("whisper-cli"); err == nil {
		return p
	}
	for _, p := range []string{
		"/opt/homebrew/bin/whisper-cli", // Apple Silicon Homebrew
		"/usr/local/bin/whisper-cli",    // Intel Homebrew
	} {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	return ""
}

// transcribeWAV runs `whisper-cli` against the given WAV and parses the JSON
// output into speech segments. whisper-cli writes JSON to "<path>.json"; we
// read that file and delete it to avoid cluttering the recordings folder.
// `whisperBin` is the absolute path to the binary (see findWhisperCli).
func transcribeWAV(wavPath, whisperBin string) ([]segment, error) {
	model := whisperModelPath()
	verbose := os.Getenv("WHISPER_VERBOSE") != ""

	// Explicit -l en avoids silence-triggered language hallucinations;
	// override via WHISPER_LANG for non-English meetings (requires a
	// multilingual model, not an .en.bin variant).
	lang := os.Getenv("WHISPER_LANG")
	if lang == "" {
		lang = "en"
	}
	args := []string{
		"-m", model,
		"-f", wavPath,
		"-l", lang,
		"--output-json",
	}
	// --no-prints installs a no-op log callback that globally suppresses
	// whisper/ggml diagnostics. Only add it when not in verbose mode,
	// otherwise WHISPER_VERBOSE=1 has no effect.
	if !verbose {
		args = append(args, "--no-prints")
	}
	// Opt-in voice-activity detection replaces the no_speech_prob filter
	// that the OpenAI API exposed but whisper-cli's JSON does not. Engages
	// only when a Silero VAD model is installed (see whisperVADModelPath).
	if vadModel := whisperVADModelPath(); vadModel != "" {
		args = append(args, "--vad", "--vad-model", vadModel, "--vad-thold", "0.5")
	}

	cmd := exec.Command(whisperBin, args...)
	if verbose {
		cmd.Stderr = os.Stderr
		cmd.Stdout = os.Stdout
	}
	// (else: stdout/stderr default to nil → discarded)

	// Capture exit error so a crashed whisper-cli (bad model, OOM, missing
	// VAD model, etc.) surfaces its failure reason instead of the generic
	// "produced no JSON" message.
	runErr := cmd.Run()

	jsonPath := wavPath + ".json"
	// Register cleanup before ReadFile. os.Remove is a no-op when the file
	// doesn't exist, so this is safe even in error paths.
	defer os.Remove(jsonPath)

	data, err := os.ReadFile(jsonPath)
	if err != nil {
		if runErr != nil {
			return nil, fmt.Errorf("whisper-cli failed (%v) and produced no JSON at %s: %w", runErr, jsonPath, err)
		}
		return nil, fmt.Errorf("whisper-cli produced no JSON at %s: %w", jsonPath, err)
	}

	var res struct {
		Transcription []struct {
			Text    string `json:"text"`
			Offsets struct {
				From int `json:"from"` // milliseconds
				To   int `json:"to"`
			} `json:"offsets"`
		} `json:"transcription"`
	}
	if err := json.Unmarshal(data, &res); err != nil {
		return nil, fmt.Errorf("parse whisper JSON: %w", err)
	}

	var segs []segment
	for _, s := range res.Transcription {
		text := strings.TrimSpace(s.Text)
		// whisper tends to emit bracketed non-speech markers like
		// "[MUSIC PLAYING]" or "[SILENCE]" during idle stretches — drop them.
		if text == "" || (strings.HasPrefix(text, "[") && strings.HasSuffix(text, "]")) {
			continue
		}
		segs = append(segs, segment{
			Start: float64(s.Offsets.From) / 1000.0,
			End:   float64(s.Offsets.To) / 1000.0,
			Text:  text,
		})
	}
	return segs, nil
}

// ── Helpers ───────────────────────────────────────────────────────────────────

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}

// ── Recording output layout ──────────────────────────────────────────────────
//
// Each meeting's three WAVs land in ~/Documents/SideHuddle/<timestamp app [— title]>/
// The Rust core writes all recordings to the base output dir with a shared
// numeric stem; after RecordingReady fires we move them into a per-meeting
// subfolder so the library remains ignorant of our naming convention.

type meetingState struct {
	started          time.Time
	app              string
	title            string          // populated from MeetingUpdated / title poller
	participantsSeen map[string]bool // unique remote-speaker names from SpeakerChanged
}

func mustOutputBaseDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		fmt.Fprintln(os.Stderr, "cannot resolve home dir:", err)
		os.Exit(1)
	}
	dir := filepath.Join(home, "Documents", "SideHuddle")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "cannot create output dir:", err)
		os.Exit(1)
	}
	return dir
}

// organizeRecording moves the three WAVs from a RecordingReady event into
// a timestamped per-meeting subfolder under baseDir and returns a new Event
// with the rewritten paths. If a move fails, the original path is preserved
// on that stream so the caller still sees a valid file location.
// organizeRecording moves the three WAVs from a RecordingReady event into
// a timestamped per-meeting subfolder under baseDir, renames them to include
// the meeting title so files are self-describing, and returns a new Event
// with the rewritten paths.
func organizeRecording(e *sh.Event, m meetingState, baseDir string) *sh.Event {
	stem := m.started.Format("2006-01-02 15-04")
	folder := stem + " " + sanitizeName(m.app)
	if m.title != "" {
		folder += " \u2014 " + sanitizeName(m.title)
	}
	dest := filepath.Join(baseDir, folder)
	if err := os.MkdirAll(dest, 0o755); err != nil {
		fmt.Fprintf(os.Stderr, "mkdir %q: %v\n", dest, err)
		return e
	}

	// Build a short, readable filename stem from the meeting title (or a
	// fallback) so the WAV files are self-describing inside the folder.
	fileStem := fileStemFromMeeting(m)

	move := func(old, suffix string) string {
		if old == "" {
			return old
		}
		newPath := filepath.Join(dest, fileStem+suffix+".wav")
		if err := os.Rename(old, newPath); err != nil {
			fmt.Fprintf(os.Stderr, "rename %q \u2192 %q: %v\n", old, newPath, err)
			return old
		}
		return newPath
	}

	out := *e
	out.Path = move(e.Path, "")
	out.OthersPath = move(e.OthersPath, "-others")
	out.SelfPath = move(e.SelfPath, "-self")
	return &out
}

// fileStemFromMeeting returns a short filename-safe stem for the three WAV
// files inside a meeting folder.  When a title is available, it is preferred
// (truncated to 50 characters) so the files are self-describing.
func fileStemFromMeeting(m meetingState) string {
	if m.title != "" {
		s := sanitizeName(m.title)
		if len([]rune(s)) > 50 {
			runes := []rune(s)
			s = strings.TrimSpace(string(runes[:50]))
		}
		if s != "" {
			return s
		}
	}
	return "recording"
}

// pollMeetingTitle scans on-screen windows every few seconds to:
//  1. Update m.title + the menu bar when a better meeting-shaped title is found.
//  2. Detect meeting-window closure as a Go-side fallback alongside the Rust
//     window watcher. Phase 1 finds the primary meeting window by ID; Phase 2
//     watches that ID for removal and calls l.StopRecording() when it's gone.
//
// The goroutine exits when `stop` is closed (i.e. when RecordingEnded fires).
func pollMeetingTitle(app string, m *meetingState, l *sh.Listener, stop <-chan struct{}) {
	ticker := time.NewTicker(3 * time.Second)
	defer ticker.Stop()

	var watchID uint32 // Phase 2: CGWindowID of the identified meeting window

	for {
		select {
		case <-stop:
			return
		case <-ticker.C:
			// ── Title update ──────────────────────────────────────────────
			// Iterate every window owned by the meeting app and pick the
			// most meeting-shaped title. CGWindowList z-order alone is
			// unreliable: a Teams chat tab the user clicked while joining
			// can win the front-most race over the actual meeting window.
			t := pickBestMeetingTitle(cocoaFindMeetingTitles(app), app)
			if t != "" && t != m.title {
				fmt.Printf("📝  title (polled): %q\n", t)
				m.title = t
				cocoaSetRecording(true, m.app, t)
			}

			// ── Window-close detection (Go-side fallback) ─────────────────
			if watchID == 0 {
				// Phase 1: find the primary meeting window by CGWindowID.
				// cocoaFindMeetingWindowID picks the largest layer-0 window
				// owned by app, mirroring the Rust window watcher's logic.
				if id := cocoaFindMeetingWindowID(app); id != 0 {
					watchID = id
					fmt.Printf("👁   watching window %d for closure\n", watchID)
				}
			} else {
				// Phase 2: has the watched window been destroyed?
				if !cocoaWindowExists(watchID) {
					fmt.Println("🔴  meeting window closed (Go-side detection) — stopping recording")
					l.StopRecording()
					return
				}
			}
		}
	}
}

// pickBestMeetingTitle scores every candidate window title and returns the
// one most likely to belong to the actual meeting window (vs a chat tab or
// app chrome). Returns "" when nothing scores positively — caller keeps
// whatever title it already had.
//
// Why this exists: the Rust window watcher and CGWindowList z-order both
// pick the front-most window, but Teams puts chat tabs and meeting windows
// in the same z-pool. If the user clicked a chat tab while joining a call,
// the chat title wins the race and ends up as the recording's name.
func pickBestMeetingTitle(titles []string, app string) string {
	bestScore := 0
	best := ""
	for _, raw := range titles {
		cleaned := filterWindowTitle(raw, app)
		if cleaned == "" {
			continue
		}
		// Score on the raw title so signals like a leading time range
		// ("9:00-10:00 …") still count — those get stripped during clean.
		s := meetingTitleScore(raw, cleaned, app)
		if s > bestScore {
			bestScore = s
			best = cleaned
		}
	}
	return best
}

// teamsChromeTabs are the top-level Teams nav tabs. A title starting with one
// of these followed by " | " is a tab title, never a meeting.
var teamsChromeTabs = []string{
	"Calendar", "Chat", "Activity", "Files", "Apps", "Teams",
	"Settings", "Search", "Help", "More", "Home",
}

// teamsTimePrefix matches "9:00", "9:00 AM", "9:00-10:00", "9:00 - 10:00 PM"
// at the start of a Teams meeting title.
var teamsTimePrefix = regexp.MustCompile(
	`^\d{1,2}:\d{2}(?:\s*(?:AM|PM))?(?:\s*[-–—]\s*\d{1,2}:\d{2}(?:\s*(?:AM|PM))?)?\s+`)

// filterWindowTitle drops window titles that are clearly NOT a meeting (chat
// tabs, app chrome) and returns a cleaned-up version of the rest. Returning ""
// means "definitely not a meeting" — caller skips it.
func filterWindowTitle(title, app string) string {
	title = strings.TrimSpace(title)
	if title == "" || title == app {
		return ""
	}
	switch app {
	case "Microsoft Teams":
		// Tab title: "<TabName> | <Context>" (e.g., "Chat | Amanda Silver",
		// "Calendar | Microsoft Teams"). Always drop.
		for _, tab := range teamsChromeTabs {
			if strings.HasPrefix(title, tab+" | ") {
				return ""
			}
		}
		// Older Teams: "<X> | Microsoft Teams". Strip the suffix; if the
		// remainder is just a chrome tab name, drop it.
		if strings.HasSuffix(title, " | Microsoft Teams") {
			prefix := strings.TrimSuffix(title, " | Microsoft Teams")
			for _, tab := range teamsChromeTabs {
				if prefix == tab {
					return ""
				}
			}
			return cleanTeamsMeetingTitle(prefix)
		}
	case "zoom.us", "Zoom":
		if title == "Zoom" || title == "Zoom Meetings" {
			return ""
		}
	}
	return title
}

// meetingTitleScore returns how likely a window title belongs to a real
// meeting (rather than a chat tab that slipped through filterWindowTitle).
// Bigger = more meeting-like. Thresholds tuned to observed Teams titles:
//
//   - Leading time range on the raw title ("9:00-10:00 …"): a Teams calendar
//     meeting window, near-certain. +20.
//   - Structural separators on the cleaned title (— / + < > ()): meeting
//     subjects almost always have one ("Internal- GE Aerospace - …",
//     "Omar (Microsoft) <> Stuart Brown + …"). +5 each, capped at +20.
//   - 4+ words: descriptive meeting name like "Project Lobster Review". +3.
//   - Bare 1–2 words with no separators (e.g. a person's name): score 1,
//     used only when nothing better is available.
func meetingTitleScore(raw, cleaned, app string) int {
	if cleaned == "" {
		return 0
	}
	score := 1
	if app == "Microsoft Teams" && teamsTimePrefix.MatchString(raw) {
		score += 20
	}
	separators := 0
	for _, r := range cleaned {
		switch r {
		case '—', '–', '/', '+', '<', '>', '(', ')':
			separators++
		}
	}
	if separators > 4 {
		separators = 4
	}
	score += separators * 5
	if words := strings.Fields(cleaned); len(words) >= 4 {
		score += 3
	}
	return score
}

// Teams decorates meeting window titles with a leading time range and
// trailing channel/room tags. Strip both so the menu bar and folder name
// show just the meaningful meeting name.
//
//	"2:05-2:30 BIC Product Day | Power Platform [Virtual] (General)"
//	→ "BIC Product Day | Power Platform"
var (
	teamsLeadingTime = regexp.MustCompile(
		`^\d{1,2}:\d{2}(?:\s*(?:AM|PM))?(?:\s*[-–—]\s*\d{1,2}:\d{2}(?:\s*(?:AM|PM))?)?\s+`)
	teamsTrailingTags = regexp.MustCompile(`(?:\s*[\[(][^\])]*[\])])+$`)
)

func cleanTeamsMeetingTitle(s string) string {
	s = teamsLeadingTime.ReplaceAllString(s, "")
	s = teamsTrailingTags.ReplaceAllString(s, "")
	return strings.TrimSpace(s)
}

// isOneOnOne returns true when the meeting appears to be a 1:1:
// - the title explicitly contains "1:1" / "1 on 1" / "1on1", OR
// - the visual-ring diarization only ever detected a single remote participant.
// In 1:1 meetings the stream separation already gives us perfect speaker
// attribution (others.wav = them, self.wav = me) with no further processing.
func isOneOnOne(m meetingState) bool {
	lower := strings.ToLower(m.title)
	if strings.Contains(lower, "1:1") ||
		strings.Contains(lower, "1 on 1") ||
		strings.Contains(lower, "1on1") {
		return true
	}
	return len(m.participantsSeen) == 1
}

// otherPersonName resolves the remote participant's display name for 1:1
// meetings.  It prefers the name that appeared in SpeakerChanged events (i.e.
// the name Teams showed on the speaking-ring tile), and falls back to
// extracting a name from the meeting title.
func otherPersonName(m meetingState) string {
	for name := range m.participantsSeen {
		return name // only one entry in a true 1:1
	}
	// Fallback: strip "1:1", "1 on 1", connector words, and the app name
	// to extract the counterpart's name from the meeting title.
	s := m.title
	s = regexp.MustCompile(`(?i)\b1[:\s-]?(?:on|o[n]?)[\s-]?1\b|1:1`).ReplaceAllString(s, "")
	s = strings.NewReplacer("with", "", "WITH", "", "With", "").Replace(s)
	s = strings.Trim(strings.TrimSpace(s), " -—–|")
	if s != "" && s != m.app {
		return s
	}
	return "Guest"
}

// myDisplayName returns the current user's macOS display name (Full Name in
// System Settings), falling back to the UNIX username.  Used to label the
// "self" audio stream in 1:1 transcripts.
func myDisplayName() string {
	if u, err := user.Current(); err == nil {
		if u.Name != "" {
			return u.Name
		}
		return u.Username
	}
	return "Me"
}


// sanitizeName removes characters that are illegal or awkward in macOS
// file paths while preserving readability.
func sanitizeName(s string) string {
	repl := strings.NewReplacer(
		"/", "-", ":", "-", "\\", "-",
		"\n", " ", "\r", " ", "\t", " ",
		"<", "(", ">", ")",
		"|", "-", "?", "", "*", "",
		"\"", "'",
	)
	return strings.TrimSpace(repl.Replace(s))
}
