// Cocoa scaffolding for the Go CLI: Accessory activation policy (no Dock
// icon, menu-bar status item only) + a running main-thread run loop so
// permission dialogs gain focus, the process doesn't bounce, and ⌘Q works.

#import <AppKit/AppKit.h>
#import <UserNotifications/UserNotifications.h>

// ARC: declared strong so the status item lives for the app's lifetime.
static NSStatusItem *gStatusItem;
static BOOL gNotifAuthRequested = NO;

void sh_cocoa_activate(void) {
    [NSApplication sharedApplication];
    // Accessory = agent-style app: no Dock icon, can own windows/menus/dialogs.
    // Use .Regular if you ever need a Dock icon; use .Prohibited only if you
    // need a silent daemon with no UI at all (no dialogs either).
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];

    // Menu bar status item
    gStatusItem = [[NSStatusBar systemStatusBar]
        statusItemWithLength:NSSquareStatusItemLength];

    NSImage *img = [NSImage imageWithSystemSymbolName:@"waveform.circle"
                              accessibilityDescription:@"SideHuddle"];
    img.template = YES; // tint-correct against light/dark menu bars
    gStatusItem.button.image = img;
    gStatusItem.button.toolTip = @"SideHuddle — listening for meetings";

    NSMenu *menu = [[NSMenu alloc] init];
    NSMenuItem *hdr = [menu addItemWithTitle:@"SideHuddle — listening"
                                      action:nil
                               keyEquivalent:@""];
    hdr.enabled = NO;
    [menu addItem:[NSMenuItem separatorItem]];
    [menu addItemWithTitle:@"Quit SideHuddle"
                    action:@selector(terminate:)
             keyEquivalent:@"q"];
    gStatusItem.menu = menu;
}

void sh_cocoa_run(void) {
    // Blocks until sh_cocoa_terminate() dispatches -terminate: back to main.
    [NSApp run];
}

void sh_cocoa_terminate(void) {
    // Safe from any thread — hop to the main queue so -terminate: runs there.
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSApp terminate:nil];
    });
}

// Post a transient local notification (banner + sound). Safe from any thread
// — UNUserNotificationCenter is thread-safe. First call triggers a system
// authorization prompt keyed on the bundle identifier; subsequent calls
// post silently if authorized, drop silently if denied.
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
