// Force macOS permission dialogs at startup instead of waiting for the lazy
// prompt at first use. Both calls are safe to issue every launch — already-
// granted paths short-circuit, and the completion block does nothing because
// status is observed via the separate PermissionStatus event stream.

#import <Foundation/Foundation.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreGraphics/CoreGraphics.h>

void sh_request_microphone(void) {
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio
                             completionHandler:^(BOOL granted) {
        (void)granted;
    }];
}

void sh_request_screen_capture(void) {
    CGRequestScreenCaptureAccess();
}
