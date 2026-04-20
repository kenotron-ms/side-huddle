// Cocoa scaffolding for the Go CLI: Accessory activation policy (no Dock
// icon, menu-bar status item only) + a running main-thread run loop so
// permission dialogs gain focus, the process doesn't bounce, and ⌘Q works.
//
// The menu queries live TCC permission status each time it opens, so the
// header text reflects reality ("ready" / "needs mic" / "needs screen")
// instead of always claiming "listening". The grant items deep-link to the
// correct System Settings panes — on Tahoe, CGRequestScreenCaptureAccess's
// redirect dialog can't hold focus from an Accessory app, so we bypass it.

#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <UserNotifications/UserNotifications.h>
#import <ServiceManagement/ServiceManagement.h>

// Forward declarations — the menu controller methods read gStatusItem, which
// is defined below. Keep the statics near the top so every later method can
// reference them without an ordering dance.
static NSStatusItem *gStatusItem;
static id            gController; // SHController on macOS 13+
static BOOL          gNotifAuthRequested = NO;

// ── Menu controller ──────────────────────────────────────────────────────────

API_AVAILABLE(macos(13.0))
@interface SHController : NSObject <NSMenuDelegate>
@property (nonatomic, weak) NSMenuItem *headerItem;
@property (nonatomic, weak) NSMenuItem *micItem;
@property (nonatomic, weak) NSMenuItem *screenItem;
@property (nonatomic, weak) NSMenuItem *loginItem;
@property (nonatomic)       BOOL       isRecording;
@property (nonatomic, copy) NSString  *currentApp;
@property (nonatomic, copy) NSString  *currentTitle;
- (void)refresh;
- (void)refreshStatusItem;
- (void)setRecording:(BOOL)rec app:(NSString *)app title:(NSString *)title;
- (void)toggleLoginItem:(id)sender;
- (void)openMicSettings:(id)sender;
- (void)openScreenRecordingSettings:(id)sender;
- (void)openDocumentsFolder:(id)sender;
@end

@implementation SHController

// Called just before the menu displays — refresh all dynamic state.
- (void)menuWillOpen:(NSMenu *)menu {
    [self refresh];
}

- (void)refresh {
    AVAuthorizationStatus micStatus =
        [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
    BOOL micGranted    = (micStatus == AVAuthorizationStatusAuthorized);
    BOOL screenGranted = CGPreflightScreenCaptureAccess();

    self.micItem.title = micGranted
        ? @"✓  Microphone granted"
        : @"Grant Microphone Access…";
    self.micItem.enabled = !micGranted;

    self.screenItem.title = screenGranted
        ? @"✓  Screen Recording granted"
        : @"Grant Screen Recording Access…";
    self.screenItem.enabled = !screenGranted;

    if (self.isRecording) {
        NSString *desc = self.currentApp.length ? self.currentApp : @"meeting";
        if (self.currentTitle.length) {
            desc = [NSString stringWithFormat:@"%@ — %@", self.currentApp, self.currentTitle];
        }
        self.headerItem.title = [NSString stringWithFormat:@"● Recording: %@", desc];
    } else if (micGranted && screenGranted) {
        self.headerItem.title = @"SideHuddle — ready";
    } else {
        NSMutableArray *missing = [NSMutableArray array];
        if (!micGranted)    [missing addObject:@"mic"];
        if (!screenGranted) [missing addObject:@"screen recording"];
        self.headerItem.title = [NSString stringWithFormat:
            @"SideHuddle — needs %@", [missing componentsJoinedByString:@" + "]];
    }

    SMAppService *svc = [SMAppService mainAppService];
    self.loginItem.state = (svc.status == SMAppServiceStatusEnabled)
        ? NSControlStateValueOn
        : NSControlStateValueOff;
}

- (void)setRecording:(BOOL)rec app:(NSString *)app title:(NSString *)title {
    self.isRecording  = rec;
    self.currentApp   = app   ?: @"";
    self.currentTitle = title ?: @"";
    [self refreshStatusItem];
    [self refresh]; // update the menu header even when menu isn't open
}

// Updates the menu-bar status item icon. Red record-dot while recording,
// template waveform otherwise. Meeting details live in the dropdown menu
// header — keeping the menu bar tight.
- (void)refreshStatusItem {
    NSImage *img;
    if (self.isRecording) {
        img = [NSImage imageWithSystemSymbolName:@"record.circle.fill"
                        accessibilityDescription:@"Recording"];
        if (@available(macOS 12.0, *)) {
            NSImageSymbolConfiguration *cfg = [NSImageSymbolConfiguration
                configurationWithPaletteColors:@[NSColor.systemRedColor]];
            img = [img imageWithSymbolConfiguration:cfg];
        }
        img.template = NO; // preserve the red tint
    } else {
        img = [NSImage imageWithSystemSymbolName:@"waveform.circle"
                        accessibilityDescription:@"SideHuddle"];
        img.template = YES; // tint with menu bar
    }
    gStatusItem.button.image = img;
    gStatusItem.button.title = @""; // always empty — title belongs in the dropdown
}

- (void)toggleLoginItem:(id)sender {
    SMAppService *svc = [SMAppService mainAppService];
    NSError *err = nil;
    if (svc.status == SMAppServiceStatusEnabled) {
        [svc unregisterAndReturnError:&err];
    } else {
        [svc registerAndReturnError:&err];
    }
    if (err) NSLog(@"Launch-at-Login toggle failed: %@", err);
    [self refresh];
}

- (void)openMicSettings:(id)sender {
    [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:
        @"x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"]];
}

- (void)openScreenRecordingSettings:(id)sender {
    // Short-circuit if already granted — just open Settings so the user can
    // view / revoke. Running tccutil reset here would wipe the grant, which
    // is exactly what the user doesn't want.
    if (CGPreflightScreenCaptureAccess()) {
        [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:
            @"x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"]];
        return;
    }

    // Force the classic TCC modal ("Open System Settings / Deny") to appear:
    //
    // 1. `tccutil reset` — restores this bundle's ScreenCapture state to
    //    NotDetermined. CGRequestScreenCaptureAccess only shows a dialog
    //    on NotDetermined; a prior self-dismissed attempt left Denied, which
    //    suppresses subsequent dialogs.
    // 2. Bump to Regular activation policy so the dialog can hold focus —
    //    Accessory apps on Tahoe cannot own TCC modals.
    // 3. SCShareableContent (ScreenCaptureKit) triggers TCC classification.
    // 4. CGRequestScreenCaptureAccess presents the modal.
    // 5. Fallback: open Settings after 2s in case the modal still doesn't
    //    appear (e.g. if Tahoe decides against it for any reason).

    NSString *bid = [[NSBundle mainBundle] bundleIdentifier];
    if (bid.length) {
        NSTask *task = [[NSTask alloc] init];
        task.launchPath = @"/usr/bin/tccutil";
        task.arguments = @[@"reset", @"ScreenCapture", bid];
        task.standardOutput = [NSPipe pipe];
        task.standardError  = [NSPipe pipe];
        @try { [task launch]; [task waitUntilExit]; }
        @catch (NSException *e) { /* ignore */ }
    }

    [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
    [NSApp activateIgnoringOtherApps:YES];

    dispatch_async(dispatch_get_main_queue(), ^{
        if (@available(macOS 12.3, *)) {
            [SCShareableContent getShareableContentWithCompletionHandler:
                ^(SCShareableContent * _Nullable content, NSError * _Nullable error) {
                    (void)content; (void)error;
                }];
        }
        CGRequestScreenCaptureAccess();
    });

    // Fallback: 2s later, deep-link into Settings so the user has a
    // guaranteed path forward even if the modal still doesn't appear.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC),
                   dispatch_get_main_queue(), ^{
        [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:
            @"x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"]];
    });

    // Drop back to Accessory after 30s — generous enough for the user to
    // finish the grant in Settings.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 30 * NSEC_PER_SEC),
                   dispatch_get_main_queue(), ^{
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    });
}

- (void)openDocumentsFolder:(id)sender {
    NSURL *home = [NSFileManager.defaultManager homeDirectoryForCurrentUser];
    NSURL *dir  = [home URLByAppendingPathComponent:@"Documents/SideHuddle"];
    [NSFileManager.defaultManager createDirectoryAtURL:dir
                           withIntermediateDirectories:YES
                                            attributes:nil
                                                 error:nil];
    [[NSWorkspace sharedWorkspace] openURL:dir];
}

@end

// ── Exported C entry points ─────────────────────────────────────────────────

void sh_cocoa_activate(void) {
    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];

    gStatusItem = [[NSStatusBar systemStatusBar]
        statusItemWithLength:NSSquareStatusItemLength];

    NSImage *img = [NSImage imageWithSystemSymbolName:@"waveform.circle"
                              accessibilityDescription:@"SideHuddle"];
    img.template = YES;
    gStatusItem.button.image = img;
    gStatusItem.button.toolTip = @"SideHuddle — click for menu";

    NSMenu *menu = [[NSMenu alloc] init];

    NSMenuItem *header = [menu addItemWithTitle:@"SideHuddle"
                                         action:nil
                                  keyEquivalent:@""];
    header.enabled = NO;
    [menu addItem:[NSMenuItem separatorItem]];

    if (@available(macOS 13.0, *)) {
        SHController *c = [[SHController alloc] init];

        NSMenuItem *micItem = [menu addItemWithTitle:@"Grant Microphone Access…"
                                              action:@selector(openMicSettings:)
                                       keyEquivalent:@""];
        micItem.target = c;

        NSMenuItem *scrItem = [menu addItemWithTitle:@"Grant Screen Recording Access…"
                                              action:@selector(openScreenRecordingSettings:)
                                       keyEquivalent:@""];
        scrItem.target = c;

        [menu addItem:[NSMenuItem separatorItem]];

        NSMenuItem *docItem = [menu addItemWithTitle:@"Open Recordings Folder"
                                              action:@selector(openDocumentsFolder:)
                                       keyEquivalent:@""];
        docItem.target = c;

        [menu addItem:[NSMenuItem separatorItem]];

        NSMenuItem *loginItem = [menu addItemWithTitle:@"Launch at Login"
                                                action:@selector(toggleLoginItem:)
                                         keyEquivalent:@""];
        loginItem.target = c;

        [menu addItem:[NSMenuItem separatorItem]];

        c.headerItem = header;
        c.micItem    = micItem;
        c.screenItem = scrItem;
        c.loginItem  = loginItem;
        menu.delegate = c;
        gController = c;
        [c refresh];
    }

    [menu addItemWithTitle:@"Quit SideHuddle"
                    action:@selector(terminate:)
             keyEquivalent:@"q"];
    gStatusItem.menu = menu;
}

void sh_cocoa_run(void)       { [NSApp run]; }

void sh_cocoa_terminate(void) {
    dispatch_async(dispatch_get_main_queue(), ^{ [NSApp terminate:nil]; });
}

// Scan on-screen windows for a meeting-looking title owned by `app`.
// Returns a heap-allocated UTF-8 string (caller must free) or NULL.
// Requires Screen Recording permission to read window titles — which we have
// by the time we're recording, so the scan is reliable from that point on.
const char *sh_cocoa_find_meeting_title(const char *app) {
    if (!app) return NULL;
    NSString *appName = [NSString stringWithUTF8String:app];

    CFArrayRef windows = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    if (!windows) return NULL;

    NSArray *chromeTitles = @[@"Calendar", @"Chat", @"Activity", @"Files",
                              @"Apps", @"Teams", @"Settings", @"Search",
                              @"Help", @"More", @"Home"];

    NSString *best = nil;
    CFIndex n = CFArrayGetCount(windows);
    for (CFIndex i = 0; i < n; i++) {
        NSDictionary *w = (__bridge NSDictionary *)CFArrayGetValueAtIndex(windows, i);
        NSString *ownerName = w[(id)kCGWindowOwnerName];
        NSString *title     = w[(id)kCGWindowName];
        if (![ownerName isEqualToString:appName]) continue;
        if (title.length == 0)                    continue;
        if ([title isEqualToString:appName])      continue;

        if ([appName isEqualToString:@"Microsoft Teams"] &&
            [title hasSuffix:@" | Microsoft Teams"]) {
            NSString *prefix = [title substringToIndex:
                title.length - @" | Microsoft Teams".length];
            if ([chromeTitles containsObject:prefix]) continue;
        }

        best = title;
        break;
    }
    CFRelease(windows);

    if (!best) return NULL;
    return strdup([best UTF8String]);
}

// Set/clear the menu-bar recording indicator. Thread-safe — hops to main.
void sh_cocoa_set_recording(int recording, const char *app, const char *title) {
    BOOL rec = recording != 0;
    NSString *appStr   = app   ? [NSString stringWithUTF8String:app]   : @"";
    NSString *titleStr = title ? [NSString stringWithUTF8String:title] : @"";

    dispatch_async(dispatch_get_main_queue(), ^{
        if (@available(macOS 13.0, *)) {
            SHController *c = (SHController *)gController;
            [c setRecording:rec app:appStr title:titleStr];
        }
    });
}

void sh_cocoa_notify(const char *title, const char *body) {
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];

    if (!gNotifAuthRequested) {
        gNotifAuthRequested = YES;
        [center requestAuthorizationWithOptions:UNAuthorizationOptionAlert | UNAuthorizationOptionSound
                              completionHandler:^(BOOL granted, NSError * _Nullable error) {
            (void)granted; (void)error;
        }];
    }

    UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
    content.title = [NSString stringWithUTF8String:title];
    content.body  = [NSString stringWithUTF8String:body];
    content.sound = [UNNotificationSound defaultSound];

    UNNotificationRequest *req = [UNNotificationRequest
        requestWithIdentifier:[[NSUUID UUID] UUIDString]
                      content:content
                      trigger:nil];
    [center addNotificationRequest:req withCompletionHandler:^(NSError * _Nullable error) {
        (void)error;
    }];
}
