//go:build !darwin

package main

// No-op shims so main.go compiles unchanged on non-darwin targets. The Cocoa
// scaffolding exists solely to give macOS a main-thread run loop for TCC
// permission dialogs and Dock activation — irrelevant elsewhere.

func cocoaActivate()                    {}
func cocoaRun()                         { select {} } // block forever; listener runs elsewhere
func cocoaTerminate()                   {}
func cocoaNotify(_, _ string)           {}
func cocoaSetRecording(_ bool, _, _ string) {}
func cocoaFindMeetingTitle(_ string) string  { return "" }
