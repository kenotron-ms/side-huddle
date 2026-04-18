// Command demo — side-huddle Go demo (CGo-backed).
    //
    // Demonstrates the full event-emitter lifecycle using the Rust cdylib via CGo.
    //
    // Usage:
    //
    //	make run-demo
    //	# or directly (after cargo build --release):
    //	DYLD_LIBRARY_PATH=crates/side-huddle/target/release go run ./cmd/demo
    package main

    import (
    	"bufio"
    	"fmt"
    	"os"
    	"os/signal"
    	"strings"
    	"syscall"

    	sh "github.com/kenotron-ms/side-huddle/bindings/go"
    )

    func main() {
    	fmt.Printf("side-huddle %s — waiting for Teams / Zoom / Google Meet…\n\n", sh.Version())

    	listener := sh.New()

    	// ── Handler 1: log every lifecycle event ──────────────────────────────
    	listener.On(func(e *sh.Event) {
    		switch e.Kind {
    		case sh.PermissionStatus:
    			icon := map[sh.PermStatus]string{
    				sh.Granted:      "✅",
    				sh.NotRequested: "⏳",
    				sh.Denied:       "❌",
    			}[e.PermStatus]
    			fmt.Printf("%s  permission: %v → %v\n", icon, e.Permission, e.PermStatus)
    		case sh.PermissionsGranted:
    			fmt.Println("✅  all permissions granted")
    		case sh.MeetingDetected:
    			fmt.Printf("🟢  detected:  %s\n", e.App)
    		case sh.MeetingUpdated:
    			fmt.Printf("📋  updated:   %s — %q\n", e.App, e.Title)
    		case sh.RecordingStarted:
    			fmt.Printf("⏺   recording: %s started\n", e.App)
    		case sh.MeetingEnded:
    			fmt.Printf("🔴  ended:     %s\n", e.App)
    		case sh.RecordingEnded:
    			fmt.Printf("⏹   recording: %s stopped\n", e.App)
    		case sh.RecordingReady:
    			fmt.Printf("💾  saved:     %s → %s\n", e.App, e.Path)
    		case sh.Error:
    			fmt.Fprintf(os.Stderr, "⚠️   error:     %s\n", e.Message)
    		}
    	})

    	// ── Handler 2: prompt user before recording ───────────────────────────
    	listener.On(func(e *sh.Event) {
    		if e.Kind != sh.MeetingDetected {
    			return
    		}
    		fmt.Printf("   Record %s? [y/N] ", e.App)
    		scanner := bufio.NewScanner(os.Stdin)
    		if scanner.Scan() && strings.EqualFold(strings.TrimSpace(scanner.Text()), "y") {
    			listener.Record()
    		}
    	})

    	if err := listener.Start(); err != nil {
    		fmt.Fprintln(os.Stderr, "failed to start:", err)
    		os.Exit(1)
    	}
    	defer listener.Stop()

    	fmt.Println("monitoring… (Ctrl-C to exit)")

    	// Block until Ctrl-C
    	quit := make(chan os.Signal, 1)
    	signal.Notify(quit, os.Interrupt, syscall.SIGTERM)
    	<-quit
    	fmt.Println("\nshutting down…")
    }
    