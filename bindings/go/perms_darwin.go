//go:build darwin

package sidehuddle

/*
extern void sh_request_microphone(void);
extern void sh_request_screen_capture(void);
*/
import "C"

// RequestMicrophone kicks the macOS microphone authorization dialog now,
// instead of waiting for the first record() call to do it lazily. Returns
// immediately; observe the granted/denied result via the PermissionStatus
// events that fire once Start() runs. Safe to call on every launch.
func RequestMicrophone() { C.sh_request_microphone() }

// RequestScreenCapture nudges macOS to show the Screen Recording permission
// affordance for the *calling bundle*. On macOS 15+ this shows a system
// dialog that routes the user to Settings. On 14.x it is effectively a probe
// and the user must manually toggle the app in Settings. Harmless to call
// when access is already granted.
func RequestScreenCapture() { C.sh_request_screen_capture() }
