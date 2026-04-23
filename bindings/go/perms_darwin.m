// Force macOS permission dialogs at startup instead of waiting for the lazy
    // prompt at first use.
    //
    // macOS 26 (Sequoia/Tahoe) TCC attribution + registration fixes
    // ────────────────────────────────────────────────────────────────
    // 1. CGRequestScreenCaptureAccess() is deprecated on macOS 15+ and no longer
    //    reliably adds the app to System Settings → Screen & System Audio Recording.
    //    The correct trigger is SCShareableContent (ScreenCaptureKit): calling
    //    getShareableContent forces TCC to register the app and present the dialog.
    //
    // 2. When an NSApp with Accessory activation policy requests a privacy
    //    permission from a background thread, macOS 26 attributes the dialog to
    //    the frontmost REGULAR app rather than the requesting process. Fix: dispatch
    //    to the main queue and temporarily become a Regular app so TCC attributes
    //    the dialog correctly, then revert to Accessory.

    #import <Foundation/Foundation.h>
    #import <AppKit/AppKit.h>
    #import <AVFoundation/AVFoundation.h>
    #import <CoreGraphics/CoreGraphics.h>
    #import <ScreenCaptureKit/ScreenCaptureKit.h>

    void sh_request_microphone(void) {
        dispatch_async(dispatch_get_main_queue(), ^{
            // Become a visible Regular app so macOS attributes the dialog to us,
            // not to whatever app happens to be frontmost (e.g. Loom / iTerm2).
            [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
            [NSApp activateIgnoringOtherApps:YES];

            [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio
                                     completionHandler:^(BOOL granted) {
                (void)granted;
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

            // SCShareableContent is the correct macOS 15+ API for triggering Screen
            // Recording TCC registration. CGRequestScreenCaptureAccess() is deprecated
            // and no longer reliably adds the app to the System Settings list.
            if (@available(macOS 12.3, *)) {
                [SCShareableContent getShareableContentWithCompletionHandler:
                    ^(SCShareableContent *content, NSError *error) {
                        (void)content; (void)error;
                        // Revert to Accessory once the prompt has been handled.
                        dispatch_async(dispatch_get_main_queue(), ^{
                            [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
                        });
                }];
            } else {
                // Fallback for macOS < 12.3 (below our minimum, but be safe)
                CGRequestScreenCaptureAccess();
                dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC),
                               dispatch_get_main_queue(), ^{
                    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
                });
            }
        });
    }
    