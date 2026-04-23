// Force macOS permission dialogs at startup instead of waiting for the lazy
// prompt at first use.
//
// macOS 26 (Sequoia) TCC attribution fix
// ───────────────────────────────────────
// When an NSApplication with Accessory activation policy requests a privacy
// permission from a background thread, macOS 26 attributes the dialog to the
// FRONTMOST REGULAR APP rather than the requesting process. For a menu-bar
// agent that is usually behind another app (Loom, iTerm2, etc.), this shows
// "Loom wants to access the microphone" instead of "SideHuddle".
//
// Fix: dispatch permission requests to the main queue and temporarily bump the
// activation policy to NSApplicationActivationPolicyRegular so the OS correctly
// attributes the TCC dialog to this process.  We revert to Accessory afterwards
// so the Dock icon disappears again.

#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreGraphics/CoreGraphics.h>

void sh_request_microphone(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        // Become a visible Regular app so macOS attributes the dialog to us,
        // not to whatever app happens to be frontmost (e.g. Loom / iTerm2).
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        [NSApp activateIgnoringOtherApps:YES];

        [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio
                                 completionHandler:^(BOOL granted) {
            (void)granted;
            // Revert to Accessory once the user has responded so we stop
            // occupying a Dock slot.
            dispatch_async(dispatch_get_main_queue(), ^{
                [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
            });
        }];
    });
}

void sh_request_screen_capture(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        [NSApp activateIgnoringOtherApps:YES];
        CGRequestScreenCaptureAccess();
        // Revert after a short delay to allow the Settings deep-link to open
        // before we disappear from the Dock again.
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC),
                       dispatch_get_main_queue(), ^{
            [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        });
    });
}
