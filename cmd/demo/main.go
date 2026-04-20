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
	"path/filepath"
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

	fmt.Printf("side-huddle %s — waiting for Teams / Zoom / Google Meet…\n\n", sh.Version())

	// Proactively surface both macOS permission dialogs at launch so the user
	// grants once on first run instead of mid-meeting. On already-granted
	// paths these are no-ops.
	sh.RequestScreenCapture()
	sh.RequestMicrophone()

	listener := sh.New()

	wavReady := make(chan *sh.Event, 1)
	var timeline []speakerEntry
	var recordingStarted time.Time

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
			ans := prompt("   Record? [Y/n] ")
			if strings.EqualFold(ans, "n") {
				fmt.Println("   skipping.")
				return
			}
			recordingStarted = time.Now()
			listener.Record()

		case sh.RecordingStarted:
			fmt.Println("⏺   recording…")

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

		case sh.RecordingReady:
			cocoaNotify("Recording saved", filepath.Base(e.Path))
			select {
			case wavReady <- e:
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

	select {
	case ev := <-wavReady:
		fmt.Printf("💾  saved:\n")
		fmt.Printf("    mixed  → %s\n", ev.Path)
		fmt.Printf("    others → %s\n", ev.OthersPath)
		fmt.Printf("    self   → %s\n\n", ev.SelfPath)
		printTimeline(timeline, recordingStarted)
		offerTranscription(ev, timeline, recordingStarted)
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

func offerTranscription(ev *sh.Event, timeline []speakerEntry, recStart time.Time) {
	apiKey := os.Getenv("OPENAI_API_KEY")
	if apiKey == "" {
		fmt.Println("(set OPENAI_API_KEY to transcribe)")
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
			segs, err := transcribeWAV(r.path, apiKey)
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
