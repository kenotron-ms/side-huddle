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
	"path/filepath"
	"regexp"
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

	// Surface the microphone dialog at launch — AVFoundation's request API is
	// reliable and shows an inline Allow button. Screen Recording is NOT
	// auto-prompted: CGRequestScreenCaptureAccess's redirect dialog on Tahoe
	// cannot hold focus in an Accessory app and silently self-dismisses.
	// The menu bar exposes a "Grant Screen Recording Access…" item that
	// deep-links to Settings where the grant actually sticks.
	sh.RequestMicrophone()

	listener := sh.New()

	// Recordings go under ~/Documents/SideHuddle; each meeting gets its own
	// subfolder (created after RecordingReady — see organizeRecording).
	baseDir := mustOutputBaseDir()
	listener.SetOutputDir(baseDir)

	wavReady := make(chan *sh.Event, 1)
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
			cocoaNotify("Meeting detected", e.App+" — recording")
			meeting = meetingState{started: time.Now(), app: e.App}
			ans := prompt("   Record? [Y/n] ")
			if strings.EqualFold(ans, "n") {
				fmt.Println("   skipping.")
				return
			}
			recordingStarted = time.Now()
			listener.Record()

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
			titleStop = make(chan struct{})
			go pollMeetingTitle(meeting.app, &meeting, titleStop)

		case sh.SpeakerChanged:
			entry := speakerEntry{at: time.Now(), speakers: e.Speakers}
			timeline = append(timeline, entry)
			offset := time.Since(recordingStarted).Round(time.Millisecond)
			if len(e.Speakers) == 0 {
				fmt.Printf("   🔇 [%s] silence\n", fmtOffset(offset))
			} else {
				fmt.Printf("   🎤 [%s] %s\n", fmtOffset(offset), strings.Join(e.Speakers, " + "))
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
			cocoaNotify("Recording saved", filepath.Base(filepath.Dir(organized.Path)))
			select {
			case wavReady <- organized:
			default:
			}

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
	// a one-shot. Each RecordingReady prints the save summary + optional
	// transcription, resets per-meeting state, and waits for the next meeting.
	// Only ⌘Q (→ cocoaTerminate) or SIGINT breaks the loop.
	for {
		select {
		case ev := <-wavReady:
			fmt.Printf("💾  saved:\n")
			fmt.Printf("    mixed  → %s\n", ev.Path)
			fmt.Printf("    others → %s\n", ev.OthersPath)
			fmt.Printf("    self   → %s\n\n", ev.SelfPath)
			printTimeline(timeline, recordingStarted)
			offerTranscription(ev, timeline, recordingStarted)
			timeline = timeline[:0] // clear for the next meeting
		case <-quit:
			fmt.Println("\nshutting down…")
			return
		}
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

func offerTranscription(ev *sh.Event, timeline []speakerEntry, recStart time.Time) {
	if _, err := exec.LookPath("whisper-cli"); err != nil {
		fmt.Println("(install whisper-cpp to enable local transcription: brew install whisper-cpp)")
		return
	}
	if _, err := os.Stat(whisperModelPath()); err != nil {
		fmt.Printf("(whisper model not found at %s — download a .bin from huggingface.co/ggerganov/whisper.cpp)\n", whisperModelPath())
		return
	}

	ans := prompt("Transcribe? [Y/n] ")
	if strings.EqualFold(ans, "n") {
		return
	}

	streams := []struct{ label, path string }{
		{"mixed", ev.Path},
		{"others", ev.OthersPath},
		{"self", ev.SelfPath},
	}
	ch := make(chan transcriptResult, len(streams))

	for _, r := range streams {
		r := r
		go func() {
			fi, err := os.Stat(r.path)
			if err != nil || fi.Size() < minWAVBytes {
				ch <- transcriptResult{r.label, r.path, nil, nil}
				return
			}
			fmt.Printf("📝  transcribing %s…\n", r.label)
			segs, err := transcribeWAV(r.path)
			ch <- transcriptResult{r.label, r.path, segs, err}
		}()
	}

	results := map[string][]segment{}
	paths := map[string]string{}
	for _, r := range streams {
		paths[r.label] = r.path
	}
	for range streams {
		r := <-ch
		if r.err != nil {
			fmt.Fprintf(os.Stderr, "⚠️   transcription failed (%s): %v\n", r.label, r.err)
			continue
		}
		if len(r.segments) == 0 {
			continue
		}
		// Write plain-text version alongside the WAV
		txtPath := strings.TrimSuffix(r.path, ".wav") + ".txt"
		var sb strings.Builder
		for _, s := range r.segments {
			fmt.Fprintf(&sb, "[%s] %s\n", fmtSecs(s.Start), strings.TrimSpace(s.Text))
		}
		_ = os.WriteFile(txtPath, []byte(sb.String()), 0644)
		fmt.Printf("✅  %s → %s\n", r.label, txtPath)
		results[r.label] = r.segments
	}

	fmt.Println()
	printed := false
	for _, label := range []string{"mixed", "others", "self"} {
		segs, ok := results[label]
		if !ok {
			continue
		}
		fmt.Printf("── Transcript (%s) %s\n", label, strings.Repeat("─", max(0, 38-len(label))))
		for _, s := range segs {
			speaker := ""
			if label == "mixed" && len(timeline) > 0 {
				speaker = speakerAt(timeline, recStart, s.Start, s.End)
				if speaker != "" {
					speaker = " <" + speaker + ">"
				}
			} else if label == "self" {
				speaker = " <me>"
			}
			fmt.Printf("  [%s]%s %s\n", fmtSecs(s.Start), speaker, strings.TrimSpace(s.Text))
		}
		printed = true
	}
	if !printed {
		fmt.Println("(no transcript — audio may have been too short)")
	}
	fmt.Println(strings.Repeat("─", 57))
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

// transcribeWAV runs `whisper-cli` against the given WAV and parses the JSON
// output into speech segments. whisper-cli writes JSON to "<path>.json"; we
// read that file and delete it to avoid cluttering the recordings folder.
func transcribeWAV(wavPath string) ([]segment, error) {
	model := whisperModelPath()

	// --output-json writes "<wavPath>.json"; --no-prints silences progress
	// chatter on stderr. Explicit -l en avoids silence-triggered language
	// hallucinations; override via env for non-English meetings.
	lang := os.Getenv("WHISPER_LANG")
	if lang == "" {
		lang = "en"
	}
	cmd := exec.Command("whisper-cli",
		"-m", model,
		"-f", wavPath,
		"-l", lang,
		"--output-json",
		"--no-prints",
	)
	// whisper-cli writes Metal init logs to stderr even with --no-prints;
	// suppress unless the caller wants diagnostics via WHISPER_VERBOSE=1.
	if os.Getenv("WHISPER_VERBOSE") == "" {
		cmd.Stderr = nil
		cmd.Stdout = nil
	} else {
		cmd.Stderr = os.Stderr
		cmd.Stdout = os.Stdout
	}

	// Run and ignore exit code — whisper-cli occasionally exits nonzero even
	// on successful transcriptions. What matters is whether the JSON file
	// exists afterwards with valid content.
	_ = cmd.Run()

	jsonPath := wavPath + ".json"
	data, err := os.ReadFile(jsonPath)
	if err != nil {
		return nil, fmt.Errorf("whisper-cli produced no JSON at %s: %w", jsonPath, err)
	}
	defer os.Remove(jsonPath)

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

func prompt(question string) string {
	fmt.Print(question)
	var line string
	fmt.Scanln(&line)
	return strings.TrimSpace(line)
}

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
	started time.Time
	app     string
	title   string // populated from MeetingUpdated if that event arrives
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
func organizeRecording(e *sh.Event, m meetingState, baseDir string) *sh.Event {
	stem := m.started.Format("2006-01-02 15-04")
	folder := stem + " " + sanitizeName(m.app)
	if m.title != "" {
		folder += " — " + sanitizeName(m.title)
	}
	dest := filepath.Join(baseDir, folder)
	if err := os.MkdirAll(dest, 0o755); err != nil {
		fmt.Fprintf(os.Stderr, "mkdir %q: %v\n", dest, err)
		return e
	}

	move := func(old string) string {
		if old == "" {
			return old
		}
		newPath := filepath.Join(dest, filepath.Base(old))
		if err := os.Rename(old, newPath); err != nil {
			fmt.Fprintf(os.Stderr, "rename %q → %q: %v\n", old, newPath, err)
			return old
		}
		return newPath
	}

	out := *e
	out.Path = move(e.Path)
	out.OthersPath = move(e.OthersPath)
	out.SelfPath = move(e.SelfPath)
	return &out
}

// pollMeetingTitle scans on-screen windows every few seconds looking for a
// non-chrome window owned by the meeting app, and updates `m.title` + the
// menu bar when it finds a better name. Terminates when `stop` is closed.
//
// Needed because the Rust core's window watcher emits MeetingUpdated exactly
// once (whichever window was first enumerated) — often the Teams chrome tab
// that happened to be frontmost, not the actual meeting window.
func pollMeetingTitle(app string, m *meetingState, stop <-chan struct{}) {
	ticker := time.NewTicker(3 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-stop:
			return
		case <-ticker.C:
			raw := cocoaFindMeetingTitle(app)
			t := filterWindowTitle(raw, app)
			if t == "" || t == m.title {
				continue
			}
			fmt.Printf("📝  title (polled): %q\n", t)
			m.title = t
			cocoaSetRecording(true, m.app, t)
		}
	}
}

// filterWindowTitle drops window titles that are clearly app chrome (e.g.,
// the Teams main window showing "Calendar | Microsoft Teams") and returns
// the original title otherwise. Returning "" means "no useful title" — the
// caller keeps whatever meeting name it already had.
//
// The window watcher in the Rust core grabs the first visible window from
// the meeting app's process; if the user has Teams open on the Calendar tab
// while a call is in progress, that chrome window can win the race over the
// actual call window.
func filterWindowTitle(title, app string) string {
	title = strings.TrimSpace(title)
	if title == "" || title == app {
		return ""
	}
	switch app {
	case "Microsoft Teams":
		// Teams chrome titles take the shape "<TabName> | Microsoft Teams".
		// Drop those — real meeting windows use "<MeetingName> | Microsoft Teams"
		// but the tab names (Calendar, Chat, etc.) are enumerable; anything
		// outside this list is likely a real meeting.
		chromeTabs := []string{"Calendar", "Chat", "Activity", "Files", "Apps",
			"Teams", "Settings", "Search", "Help", "More", "Home"}
		if strings.HasSuffix(title, " | Microsoft Teams") {
			prefix := strings.TrimSuffix(title, " | Microsoft Teams")
			for _, tab := range chromeTabs {
				if prefix == tab {
					return ""
				}
			}
			// Real meeting — strip the Teams-inserted junk (time prefix,
			// bracketed channel/room tags) down to the semantic title.
			return cleanTeamsMeetingTitle(prefix)
		}
	case "zoom.us", "Zoom":
		// Zoom's main window is just "Zoom" or "Zoom Meetings"; real meeting
		// windows have the meeting ID/name.
		if title == "Zoom" || title == "Zoom Meetings" {
			return ""
		}
	}
	return title
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
