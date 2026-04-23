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
#include "_cgo_export.h"  // goRecordChoiceCallback

// Forward declarations — the menu controller methods read gStatusItem, which
// is defined below. Keep the statics near the top so every later method can
// reference them without an ordering dance.
static NSStatusItem *gStatusItem;
static id            gController; // SHController on macOS 13+
static BOOL          gNotifAuthRequested = NO;

// Notification category/action identifiers
static NSString * const kCatRecordChoice = @"SH_RECORD_CHOICE";
static NSString * const kCatOpenFolder   = @"SH_OPEN_FOLDER";
static NSString * const kActRecord       = @"RECORD";
static NSString * const kActSkip         = @"SKIP";
static NSString * const kActOpenFolder   = @"OPEN_FOLDER";

// ── Menu controller ──────────────────────────────────────────────────────────

API_AVAILABLE(macos(13.0))
@interface SHController : NSObject <NSMenuDelegate, UNUserNotificationCenterDelegate>
@property (nonatomic, weak) NSMenuItem *headerItem;
@property (nonatomic, weak) NSMenuItem *micItem;
@property (nonatomic, weak) NSMenuItem *screenItem;
@property (nonatomic, weak) NSMenuItem *loginItem;
@property (nonatomic, weak) NSMenuItem *stopItem;   // hidden except while recording
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
// UNUserNotificationCenterDelegate
- (void)userNotificationCenter:(UNUserNotificationCenter *)center
   didReceiveNotificationResponse:(UNNotificationResponse *)response
            withCompletionHandler:(void (^)(void))completionHandler;
- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
        withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler;
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
    // Show the "Stop Recording" item only while a recording is in progress.
    self.stopItem.hidden  = !rec;
    self.stopItem.enabled =  rec;
    [self refreshStatusItem];
    [self refresh]; // update the menu header even when menu isn't open
}

- (void)stopRecording:(id)sender {
    goStopRecordingCallback();
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

// ── UNUserNotificationCenterDelegate ─────────────────────────────────────────

// Always show banners even when the app is in the foreground (menu bar agents
// have no window so technically they're never "foreground", but be explicit).
- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler {
    (void)center; (void)notification;
    completionHandler(UNNotificationPresentationOptionBanner |
                      UNNotificationPresentationOptionSound);
}

// Handle action-button taps and notification-body taps.
- (void)userNotificationCenter:(UNUserNotificationCenter *)center
  didReceiveNotificationResponse:(UNNotificationResponse *)response
           withCompletionHandler:(void (^)(void))completionHandler {
    (void)center;
    NSString *action   = response.actionIdentifier;
    NSString *category = response.notification.request.content.categoryIdentifier;

    if ([category isEqualToString:kCatRecordChoice]) {
        // Tapping the notification body or the "Record" button → record.
        // Tapping "Skip" → do not record.
        int shouldRecord = 1;
        if ([action isEqualToString:kActSkip]) shouldRecord = 0;
        goRecordChoiceCallback((GoInt32)shouldRecord);

    } else if ([action isEqualToString:kActOpenFolder]) {
        NSString *folder = response.notification.request.content.userInfo[@"folder_path"];
        if (folder.length) {
            [[NSWorkspace sharedWorkspace] openURL:[NSURL fileURLWithPath:folder]];
        }
    }
    completionHandler();
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

        // ── Register notification categories ──────────────────────────────
        // Category 1: record-or-skip choice when a meeting is detected.
        UNNotificationAction *recordAct = [UNNotificationAction
            actionWithIdentifier:kActRecord
                           title:@"🔴 Record"
                         options:UNNotificationActionOptionNone];
        UNNotificationAction *skipAct = [UNNotificationAction
            actionWithIdentifier:kActSkip
                           title:@"Skip"
                         options:UNNotificationActionOptionDestructive];
        UNNotificationCategory *recordCat = [UNNotificationCategory
            categoryWithIdentifier:kCatRecordChoice
                           actions:@[recordAct, skipAct]
                 intentIdentifiers:@[]
                           options:UNNotificationCategoryOptionNone];

        // Category 2: deep-link to the meeting folder (recordings / transcripts).
        UNNotificationAction *openAct = [UNNotificationAction
            actionWithIdentifier:kActOpenFolder
                           title:@"Open Folder"
                         options:UNNotificationActionOptionNone];
        UNNotificationCategory *folderCat = [UNNotificationCategory
            categoryWithIdentifier:kCatOpenFolder
                           actions:@[openAct]
                 intentIdentifiers:@[]
                           options:UNNotificationCategoryOptionNone];

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center setNotificationCategories:[NSSet setWithObjects:recordCat, folderCat, nil]];
        center.delegate = c;     // delivers banners in foreground + handles action taps
        gNotifAuthRequested = YES;
        [center requestAuthorizationWithOptions:
            UNAuthorizationOptionAlert | UNAuthorizationOptionSound
                              completionHandler:^(BOOL granted, NSError *error) {
            (void)granted; (void)error;
        }];

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

        // "⏹ Stop Recording" — only visible while actively recording.
        NSMenuItem *stopItem = [menu addItemWithTitle:@"\u23F9  Stop Recording"
                                               action:@selector(stopRecording:)
                                        keyEquivalent:@""];
        stopItem.target = c;
        stopItem.hidden  = YES;

        [menu addItem:[NSMenuItem separatorItem]];

        c.headerItem = header;
        c.micItem    = micItem;
        c.screenItem = scrItem;
        c.loginItem  = loginItem;
        c.stopItem   = stopItem;
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
    // Auth is requested once in sh_cocoa_activate; no need to repeat it here.
    UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
    content.title = [NSString stringWithUTF8String:title];
    content.body  = [NSString stringWithUTF8String:body];
    content.sound = [UNNotificationSound defaultSound];

    UNNotificationRequest *req = [UNNotificationRequest
        requestWithIdentifier:[[NSUUID UUID] UUIDString]
                      content:content
                      trigger:nil];
    [[UNUserNotificationCenter currentNotificationCenter]
        addNotificationRequest:req withCompletionHandler:^(NSError * _Nullable error) {
            (void)error;
    }];
}

// Return the CGWindowID of the largest on-screen layer-0 window (≥10 000 px²)
// whose CGWindowOwnerName contains `app` (case-insensitive). Returns 0 if not found.
// Mirrors Rust's find_primary_window logic so both watchers agree on the window.
uint32_t sh_cocoa_find_meeting_window_id(const char *app_cstr) {
    if (!app_cstr) return 0;
    NSString *appLower = [[NSString stringWithUTF8String:app_cstr] lowercaseString];

    CFArrayRef windows = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    if (!windows) return 0;

    uint32_t bestID  = 0;
    CGFloat  bestArea = 0;
    CFIndex  n = CFArrayGetCount(windows);

    for (CFIndex i = 0; i < n; i++) {
        NSDictionary *w = (__bridge NSDictionary *)CFArrayGetValueAtIndex(windows, i);

        NSNumber *layer = w[(id)kCGWindowLayer];
        if (layer && layer.intValue != 0) continue;

        NSString *owner = [w[(id)kCGWindowOwnerName] lowercaseString];
        if (!owner || ![owner containsString:appLower]) continue;

        NSDictionary *bounds = w[(id)kCGWindowBounds];
        if (!bounds) continue;
        CGFloat area = [bounds[@"Width"] floatValue] * [bounds[@"Height"] floatValue];
        if (area < 10000.0) continue;

        if (area > bestArea) {
            bestArea = area;
            NSNumber *wid = w[(id)kCGWindowNumber];
            bestID = wid ? (uint32_t)wid.unsignedIntValue : 0;
        }
    }
    CFRelease(windows);
    return bestID;
}

// Returns 1 if a window with window_id still exists in the full window list
// (including hidden/minimized windows), 0 otherwise.
int sh_cocoa_window_exists(uint32_t window_id) {
    if (window_id == 0) return 0;
    CFArrayRef windows = CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID);
    if (!windows) return 0;

    BOOL found = NO;
    CFIndex n = CFArrayGetCount(windows);
    for (CFIndex i = 0; i < n && !found; i++) {
        NSDictionary *w = (__bridge NSDictionary *)CFArrayGetValueAtIndex(windows, i);
        NSNumber *wid = w[(id)kCGWindowNumber];
        if (wid && (uint32_t)wid.unsignedIntValue == window_id) found = YES;
    }
    CFRelease(windows);
    return found ? 1 : 0;
}

// Show a modal NSAlert asking whether to record the detected meeting.
// Primary "record?" prompt — works without notification permission and produces
// a clearly visible on-screen dialog. Dispatched to the main queue so it never
// blocks the Rust callback thread. The choice is delivered via goRecordChoiceCallback().
void sh_cocoa_show_record_alert(const char *app_cstr) {
    NSString *app = app_cstr ? [NSString stringWithUTF8String:app_cstr] : @"Meeting";
    dispatch_async(dispatch_get_main_queue(), ^{
        // Become a Regular app briefly so the alert gets focus and a Dock icon.
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        [NSApp activateIgnoringOtherApps:YES];

        NSAlert *alert = [[NSAlert alloc] init];
        alert.messageText     = @"Meeting Detected";
        alert.informativeText = [NSString stringWithFormat:
            @"%@ is active — record this meeting?", app];
        alert.alertStyle      = NSAlertStyleInformational;
        [alert addButtonWithTitle:@"\U0001F534  Record"];
        [alert addButtonWithTitle:@"Skip"];

        NSModalResponse resp = [alert runModal];
        GoInt32 choice = (resp == NSAlertFirstButtonReturn) ? 1 : 0;
        goRecordChoiceCallback(choice);

        // Return to Accessory so the Dock icon disappears again.
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    });
}

// Post an actionable "Record this meeting?" notification.
// The response is delivered via goRecordChoiceCallback() on the UNCenter delegate.
void sh_cocoa_notify_record_choice(const char *app_cstr) {
    NSString *app = app_cstr ? [NSString stringWithUTF8String:app_cstr] : @"meeting";

    UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
    content.title              = @"Meeting detected";
    content.body               = [NSString stringWithFormat:@"%@ — record this meeting?", app];
    content.sound              = [UNNotificationSound defaultSound];
    content.categoryIdentifier = kCatRecordChoice;

    // Use a fixed identifier so a new detection replaces the previous one.
    UNNotificationRequest *req = [UNNotificationRequest
        requestWithIdentifier:@"sh-record-choice"
                      content:content
                      trigger:nil];
    [[UNUserNotificationCenter currentNotificationCenter]
        addNotificationRequest:req withCompletionHandler:^(NSError * _Nullable error) {
            (void)error;
    }];
}

// Post a notification with an "Open Folder" action button that deep-links to
// folder_path in Finder when tapped.  folder_path is stored in userInfo so the
// delegate can open the right folder even if multiple notifications are queued.
void sh_cocoa_notify_with_folder(const char *title_cstr,
                                  const char *body_cstr,
                                  const char *folder_cstr) {
    NSString *title  = title_cstr  ? [NSString stringWithUTF8String:title_cstr]  : @"";
    NSString *body   = body_cstr   ? [NSString stringWithUTF8String:body_cstr]   : @"";
    NSString *folder = folder_cstr ? [NSString stringWithUTF8String:folder_cstr] : @"";

    UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
    content.title              = title;
    content.body               = body;
    content.sound              = [UNNotificationSound defaultSound];
    content.categoryIdentifier = kCatOpenFolder;
    if (folder.length) {
        content.userInfo = @{@"folder_path": folder};
    }

    UNNotificationRequest *req = [UNNotificationRequest
        requestWithIdentifier:[[NSUUID UUID] UUIDString]
                      content:content
                      trigger:nil];
    [[UNUserNotificationCenter currentNotificationCenter]
        addNotificationRequest:req withCompletionHandler:^(NSError * _Nullable error) {
            (void)error;
    }];
}
