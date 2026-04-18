/**
 * darwin/window.m — CGWindowList + CoreAudio window watcher
 *
 * Finds the window whose PID has active CoreAudio input, returns its title.
 * Used by the monitor to detect meeting-specific windows and their closure.
 */

#include <CoreGraphics/CGWindow.h>
#include <CoreFoundation/CoreFoundation.h>
#include <CoreAudio/CoreAudio.h>
#include <stdlib.h>
#include <string.h>

/**
 * Find the title of the window whose PID has kAudioProcessPropertyIsRunningInput == 1
 * and whose owner name contains `owner_substr`.
 *
 * Returns a malloc'd C string or NULL. Caller frees.
 */
char* ml_window_title_for_audio_input_pid(const char *owner_substr) {
    CFArrayRef list = CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID);
    if (!list) return NULL;
    CFIndex n = CFArrayGetCount(list);
    char *result = NULL;

    for (CFIndex i = 0; i < n && !result; i++) {
        CFDictionaryRef d = (CFDictionaryRef)CFArrayGetValueAtIndex(list, i);

        /* Layer 0 only */
        CFNumberRef lr = (CFNumberRef)CFDictionaryGetValue(d, kCGWindowLayer);
        if (lr) { int l = 0; CFNumberGetValue(lr, kCFNumberIntType, &l); if (l != 0) continue; }

        /* Owner must match */
        CFStringRef owner = (CFStringRef)CFDictionaryGetValue(d, kCGWindowOwnerName);
        if (!owner) continue;
        char obuf[256] = {0};
        CFStringGetCString(owner, obuf, sizeof(obuf), kCFStringEncodingUTF8);
        if (strcasestr(obuf, owner_substr) == NULL) continue;

        /* PID */
        CFNumberRef pr = (CFNumberRef)CFDictionaryGetValue(d, kCGWindowOwnerPID);
        if (!pr) continue;
        int pid = 0; CFNumberGetValue(pr, kCFNumberIntType, &pid);
        if (pid <= 0) continue;

        /* Check CoreAudio IsRunningInput for this PID */
        AudioObjectPropertyAddress addr = {kAudioHardwarePropertyProcessObjectList,
            kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
        UInt32 sz = 0;
        AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &addr, 0, NULL, &sz);
        if (sz == 0) continue;
        AudioObjectID *objs = (AudioObjectID*)malloc(sz);
        if (!objs) continue;
        AudioObjectGetPropertyData(kAudioObjectSystemObject, &addr, 0, NULL, &sz, objs);
        UInt32 cnt = sz / sizeof(AudioObjectID);
        int hasInput = 0;
        for (UInt32 j = 0; j < cnt && !hasInput; j++) {
            AudioObjectPropertyAddress pa = {kAudioProcessPropertyPID,
                kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
            pid_t apid = 0; UInt32 ps = sizeof(apid);
            AudioObjectGetPropertyData(objs[j], &pa, 0, NULL, &ps, &apid);
            if ((int)apid != pid) continue;
            AudioObjectPropertyAddress ia = {kAudioProcessPropertyIsRunningInput,
                kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
            UInt32 r = 0; ps = sizeof(r);
            AudioObjectGetPropertyData(objs[j], &ia, 0, NULL, &ps, &r);
            hasInput = (int)r;
        }
        free(objs);
        if (!hasInput) continue;

        /* Get window title */
        CFStringRef name = (CFStringRef)CFDictionaryGetValue(d, kCGWindowName);
        char title[512] = "(active call window)";
        if (name) CFStringGetCString(name, title, sizeof(title), kCFStringEncodingUTF8);
        if (title[0]) result = strdup(title);
    }

    CFRelease(list);
    return result;
}

/** Get any titled layer-0 window title for a given PID. Caller frees. */
char* ml_window_title_for_pid(int target_pid) {
    CFArrayRef list = CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID);
    if (!list) return NULL;
    CFIndex n = CFArrayGetCount(list);
    char *result = NULL;
    for (CFIndex i = 0; i < n && !result; i++) {
        CFDictionaryRef d = (CFDictionaryRef)CFArrayGetValueAtIndex(list, i);
        CFNumberRef lr = (CFNumberRef)CFDictionaryGetValue(d, kCGWindowLayer);
        if (lr) { int l = 0; CFNumberGetValue(lr, kCFNumberIntType, &l); if (l != 0) continue; }
        CFNumberRef pr = (CFNumberRef)CFDictionaryGetValue(d, kCGWindowOwnerPID);
        if (!pr) continue;
        int pid = 0; CFNumberGetValue(pr, kCFNumberIntType, &pid);
        if (pid != target_pid) continue;
        CFStringRef name = (CFStringRef)CFDictionaryGetValue(d, kCGWindowName);
        if (!name) continue;
        char buf[512] = {0};
        if (CFStringGetCString(name, buf, sizeof(buf), kCFStringEncodingUTF8) && buf[0])
            result = strdup(buf);
    }
    CFRelease(list);
    return result;
}
