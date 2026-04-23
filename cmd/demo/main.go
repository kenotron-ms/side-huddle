// Command demo — side-huddle Go demo (CGo-backed).
//
// Usage:
//
//	make run-demo
//	OPENAI_API_KEY=sk-... make run-demo   # enables transcription after recording
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"net/textproto"
	"os"
	"os/signal"
	"os/user"
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

	// wavReadyEvent bundles a RecordingReady event with a frozen meeting
	// snapshot so the transcription goroutine sees the correct 1:1 state
	// even if a second meeting starts before the first is transcribed.
	type wavReadyEvent struct {
		ev      *sh.Event
		meeting meetingState
	}
	wavReady := make(chan wavReadyEvent, 1)
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
			meeting = meetingState{
				started:          time.Now(),
				app:              e.App,
				participantsSeen: make(map[string]bool),
			}
			// Post the notification and return immediately — blocking here would
			// stall the Rust-side CGo event dispatch and prevent MeetingEnded
			// from ever firing (recording would never stop).
			// The goroutine waits for the user's tap (or 30s timeout) then starts
			// recording.  recordingStarted is set now so SpeakerChanged offsets
			// are correct even if recording starts a few seconds later.
			recordingStarted = time.Now()
			ch := cocoaNotifyRecordChoice(e.App)
			go func() {
				if !waitRecordChoice(ch, 30*time.Second) {
					fmt.Println("   skipping (user chose Skip).")
					return
				}
				fmt.Println("   recording.")
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
			folder := filepath.Dir(organized.Path)
			cocoaNotifyWithFolder("Recording ready", filepath.Base(folder), folder)
			select {
			case wavReady <- wavReadyEvent{ev: organized, meeting: meeting}:
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
		case ready := <-wavReady:
			ev := ready.ev
			fmt.Printf("💾  saved:\n")
			fmt.Printf("    mixed  → %s\n", ev.Path)
			fmt.Printf("    others → %s\n", ev.OthersPath)
			fmt.Printf("    self   → %s\n\n", ev.SelfPath)
			printTimeline(timeline, recordingStarted)
			runTranscription(ev, ready.meeting, timeline, recordingStarted)
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

// ── Transcription ─────────────────────────────────────────────────────────────

// WAV header = 44 bytes; 0.1s at 16 kHz mono = 3200 bytes of samples → min ~3244 bytes.
const minWAVBytes = 3244

type segment struct {
	Start        float64
	End          float64
	Text         string
	NoSpeechProb float64 // Whisper confidence that this is silence / not speech
}

// Hallucination threshold: segments where Whisper is > 60% sure there's no speech
// are almost always noise-induced gibberish (often Korean/Japanese/Chinese characters).
const noSpeechThreshold = 0.6

type transcriptResult struct {
	label    string
	path     string
	segments []segment
	err      error
}

// runTranscription automatically transcribes all three WAV streams when
// OPENAI_API_KEY is set.  For 1:1 meetings it assigns speakers directly from
// the stream separation (others = them, self = me) for perfect diarization.
// For group meetings it uses the visual-ring timeline as before.
// Notifications are sent at start and completion so the user can track progress.
func runTranscription(ev *sh.Event, m meetingState, timeline []speakerEntry, recStart time.Time) {
	apiKey := os.Getenv("OPENAI_API_KEY")
	if apiKey == "" {
		fmt.Println("(set OPENAI_API_KEY to enable transcription)")
		return
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

	for _, r := range streams {
		r := r
		go func() {
			fi, err := os.Stat(r.path)
			if err != nil || fi.Size() < minWAVBytes {
				ch <- transcriptResult{r.label, r.path, nil, nil}
				return
			}
			fmt.Printf("\U0001f4dd  transcribing %s…\n", r.label)
			segs, err := transcribeWAV(r.path, apiKey)
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

func transcribeWAV(wavPath, apiKey string) ([]segment, error) {
	wavBytes, err := os.ReadFile(wavPath)
	if err != nil {
		return nil, err
	}

	var body bytes.Buffer
	w := multipart.NewWriter(&body)
	_ = w.WriteField("model", "whisper-1")
	_ = w.WriteField("response_format", "verbose_json")
	_ = w.WriteField("timestamp_granularities[]", "segment")
	// Explicit language prevents Whisper hallucinating random foreign text on silence.
	// Override with WHISPER_LANG=fr/ja/etc if the meeting is not in English.
	lang := os.Getenv("WHISPER_LANG")
	if lang == "" {
		lang = "en"
	}
	_ = w.WriteField("language", lang)

	h := make(textproto.MIMEHeader)
	h.Set("Content-Disposition", `form-data; name="file"; filename="audio.wav"`)
	h.Set("Content-Type", "audio/wav")
	part, err := w.CreatePart(h)
	if err != nil {
		return nil, err
	}
	if _, err = part.Write(wavBytes); err != nil {
		return nil, err
	}
	w.Close()

	whisperURL := os.Getenv("WHISPER_URL")
	if whisperURL == "" {
		whisperURL = "https://api.openai.com/v1/audio/transcriptions"
	}

	req, err := http.NewRequest("POST", whisperURL, &body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+apiKey)
	req.Header.Set("Content-Type", w.FormDataContentType())

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d: %s", resp.StatusCode, b)
	}

	var res struct {
		Segments []struct {
			Start        float64 `json:"start"`
			End          float64 `json:"end"`
			Text         string  `json:"text"`
			NoSpeechProb float64 `json:"no_speech_prob"`
		} `json:"segments"`
	}
	if err = json.Unmarshal(b, &res); err != nil {
		return nil, err
	}

	var segs []segment
	for _, s := range res.Segments {
		if s.NoSpeechProb >= noSpeechThreshold {
			continue // skip — Whisper thinks this is silence/noise, not speech
		}
		segs = append(segs, segment{s.Start, s.End, strings.TrimSpace(s.Text), s.NoSpeechProb})
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
			raw := cocoaFindMeetingTitle(app)
			t := filterWindowTitle(raw, app)
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
