//go:build !darwin

package sidehuddle

// RequestMicrophone is a no-op on non-darwin — no OS permission gate exists.
func RequestMicrophone() {}

// RequestScreenCapture is a no-op on non-darwin.
func RequestScreenCapture() {}
