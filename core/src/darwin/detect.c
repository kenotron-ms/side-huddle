/**
 * darwin/detect.c — CoreAudio process-level meeting detection
 *
 * Polls kAudioProcessPropertyIsRunningInput every 300ms across all
 * CoreAudio process objects whose bundle ID matches a known meeting app.
 * Reports the active PID via a callback.
 */

#include "../../include/meetinglistener.h"
#include <CoreAudio/CoreAudio.h>
#include <CoreFoundation/CoreFoundation.h>
#include <sys/types.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* ── Bundle ID substring matching ─────────────────────────────────────────── */

static const char* MEETING_BUNDLES[] = {
    "com.microsoft.teams2",
    "com.microsoft.teams",
    "us.zoom.xos",
    "com.cisco.webexmeetingsapp",
    "com.apple.FaceTime",
    "com.tinyspeck.slackmacgap",
    "com.hnc.Discord",
    /* Browsers (cross-ref with window titles for web-based meetings) */
    "com.google.chrome",
    "com.apple.Safari",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
    NULL
};

static int isMeetingBundle(const char *bid) {
    for (int i = 0; MEETING_BUNDLES[i]; i++) {
        if (strstr(bid, MEETING_BUNDLES[i])) return 1;
    }
    return 0;
}

/* ── CoreAudio process helpers ─────────────────────────────────────────────── */

static pid_t getAudioProcPID(AudioObjectID obj) {
    AudioObjectPropertyAddress addr = {kAudioProcessPropertyPID,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    pid_t pid = 0; UInt32 sz = sizeof(pid);
    AudioObjectGetPropertyData(obj, &addr, 0, NULL, &sz, &pid);
    return pid;
}

/* Returns malloc'd bundle ID string or NULL. Caller frees. */
static char* getAudioProcBundle(AudioObjectID obj) {
    AudioObjectPropertyAddress addr = {kAudioProcessPropertyBundleID,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    CFStringRef ref = NULL; UInt32 sz = sizeof(ref);
    if (AudioObjectGetPropertyData(obj, &addr, 0, NULL, &sz, &ref) != noErr || !ref)
        return NULL;
    char *buf = malloc(256);
    CFStringGetCString(ref, buf, 256, kCFStringEncodingUTF8);
    CFRelease(ref);
    return buf;
}

static int isRunningInput(AudioObjectID obj) {
    AudioObjectPropertyAddress addr = {kAudioProcessPropertyIsRunningInput,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    UInt32 r = 0, sz = sizeof(r);
    AudioObjectGetPropertyData(obj, &addr, 0, NULL, &sz, &r);
    return (int)r;
}

/**
 * Scan all CoreAudio process objects. Return the PID + bundle ID of the first
 * meeting-app process that has IsRunningInput == 1.
 * Returns 0 if none active. `bundle_out` must be at least 256 bytes.
 */
pid_t ml_darwin_poll_active_pid(char bundle_out[256]) {
    AudioObjectPropertyAddress addr = {kAudioHardwarePropertyProcessObjectList,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    UInt32 sz = 0;
    if (AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &addr, 0, NULL, &sz) != noErr || sz == 0)
        return 0;
    AudioObjectID *objs = malloc(sz);
    if (!objs) return 0;
    AudioObjectGetPropertyData(kAudioObjectSystemObject, &addr, 0, NULL, &sz, objs);
    UInt32 n = sz / sizeof(AudioObjectID);

    pid_t result = 0;
    for (UInt32 i = 0; i < n && !result; i++) {
        char *bid = getAudioProcBundle(objs[i]);
        if (!bid) continue;
        if (isMeetingBundle(bid) && isRunningInput(objs[i])) {
            result = getAudioProcPID(objs[i]);
            strncpy(bundle_out, bid, 255);
            bundle_out[255] = '\0';
        }
        free(bid);
    }
    free(objs);
    return result;
}

/** Get the CoreAudio bundle ID for a given PID. Returns 0 on failure. */
int ml_darwin_bundle_for_pid(pid_t target, char bundle_out[256]) {
    AudioObjectPropertyAddress addr = {kAudioHardwarePropertyProcessObjectList,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    UInt32 sz = 0;
    if (AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &addr, 0, NULL, &sz) != noErr || sz == 0)
        return 0;
    AudioObjectID *objs = malloc(sz);
    if (!objs) return 0;
    AudioObjectGetPropertyData(kAudioObjectSystemObject, &addr, 0, NULL, &sz, objs);
    UInt32 n = sz / sizeof(AudioObjectID);
    int found = 0;
    for (UInt32 i = 0; i < n && !found; i++) {
        if (getAudioProcPID(objs[i]) != target) continue;
        char *bid = getAudioProcBundle(objs[i]);
        if (bid) {
            strncpy(bundle_out, bid, 255);
            bundle_out[255] = '\0';
            free(bid);
            found = 1;
        }
    }
    free(objs);
    return found;
}
