/**
 * darwin/platform.c — Darwin-specific ml_platform_poll + recording dispatch
 *
 * Implements the ml_platform_poll(), ml_tap_start(), ml_mic_start() etc.
 * symbols that monitor.c calls into.
 */

#include "../../include/meetinglistener.h"
#include <CoreGraphics/CGWindow.h>
#include <CoreFoundation/CoreFoundation.h>
#include <libproc.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <pthread.h>

/* Declared in detect.c */
extern pid_t ml_darwin_poll_active_pid(char bundle_out[256]);
extern int   ml_darwin_bundle_for_pid(pid_t target, char bundle_out[256]);

/* Declared in window.m */
extern char* ml_window_title_for_pid(int pid);

/* Declared in record_tap.m */
extern void* ml_tap_context_start(int pipe_fd, int sample_rate,
                                   ml_audio_chunk_fn on_chunk, void *ctx);
extern void  ml_tap_context_stop(void *handle);

/* Declared in record_mic.m */
extern void* ml_mic_context_start(int pipe_fd, int sample_rate,
                                   ml_audio_chunk_fn on_chunk, void *ctx);
extern void  ml_mic_context_stop(void *handle);

/* ── Known apps (proc_name → friendly name) ──────────────────────────────── */

typedef struct { const char *name; const char *app; } AppEntry;

static const AppEntry KNOWN_APPS[] = {
    {"MSTeams",           "Microsoft Teams"},
    {"teams",             "Microsoft Teams"},
    {"zoom.us",           "Zoom"},
    {"zoom",              "Zoom"},
    {"Webex",             "Cisco Webex"},
    {"webex",             "Cisco Webex"},
    {"Slack",             "Slack"},
    {"Discord",           "Discord"},
    {"FaceTime",          "FaceTime"},
    {"Google Chrome",     NULL},   /* NULL = needs window title check */
    {"Safari",            NULL},
    {"Firefox",           NULL},
    {NULL, NULL}
};

static const char* appForProcName(const char *name) {
    for (int i = 0; KNOWN_APPS[i].name; i++) {
        if (strcasecmp(name, KNOWN_APPS[i].name) == 0 ||
            strncasecmp(name, KNOWN_APPS[i].name, strlen(KNOWN_APPS[i].name)) == 0) {
            return KNOWN_APPS[i].app;  /* may be NULL for browsers */
        }
    }
    return NULL;
}

/* Check if a browser window title indicates a web meeting */
static const char* appFromWindowTitle(const char *title) {
    if (!title) return NULL;
    if (strstr(title, "Google Meet") || strstr(title, "meet.google.com"))
        return "Google Meet";
    if (strstr(title, "Zoom") && strstr(title, "Meeting"))
        return "Zoom";
    return NULL;
}

/* ── ml_platform_poll ────────────────────────────────────────────────────── */

/**
 * Called by monitor.c every 300ms.
 * Returns the PID of the active meeting app, or 0 if none.
 * Fills app_out with the friendly name (e.g. "Microsoft Teams").
 */
int ml_platform_poll(char app_out[64], char bundle_out[256]) {
    char bundle[256] = {0};
    pid_t pid = ml_darwin_poll_active_pid(bundle);
    if (pid == 0) return 0;

    /* Get process name */
    char procName[64] = {0};
    proc_name((int)pid, procName, sizeof(procName));

    const char *app = appForProcName(procName);

    if (app == NULL) {
        /* Browser — check window title */
        char *title = ml_window_title_for_pid((int)pid);
        app = appFromWindowTitle(title);
        free(title);
    }

    if (app == NULL) return 0;

    strncpy(app_out,    app,    63);  app_out[63]    = '\0';
    strncpy(bundle_out, bundle, 255); bundle_out[255] = '\0';
    return (int)pid;
}

/* ── Recording: pipe-based API ───────────────────────────────────────────── */

/* Each handle stores: context + read fd */
typedef struct { void *ctx; int read_fd; } RecHandle;

/* Pipe reader thread: reads from the pipe, calls on_chunk */
typedef struct {
    int             read_fd;
    int             sample_rate;
    int             chunk_ms;
    ml_audio_chunk_fn on_chunk;
    void           *chunk_ctx;
    volatile int    running;
} PipeReader;

static void* pipe_reader_fn(void *arg) {
    PipeReader *pr = (PipeReader*)arg;
    int frames_per_chunk = pr->sample_rate * pr->chunk_ms / 1000;
    int bytes_per_chunk  = frames_per_chunk * 2;  /* PCM-16 */
    int16_t *buf = (int16_t*)malloc(bytes_per_chunk * 8); /* 8x buffer */
    int accumulated = 0;

    while (pr->running) {
        int want = bytes_per_chunk * 8 - accumulated;
        ssize_t got = read(pr->read_fd, (char*)buf + accumulated, want);
        if (got <= 0) break;
        accumulated += (int)got;

        while (accumulated >= bytes_per_chunk) {
            pr->on_chunk(buf, frames_per_chunk, pr->sample_rate, pr->chunk_ctx);
            memmove(buf, (char*)buf + bytes_per_chunk, accumulated - bytes_per_chunk);
            accumulated -= bytes_per_chunk;
        }
    }
    free(buf);
    free(pr);
    return NULL;
}

/* ── ml_tap_start / ml_tap_stop ─────────────────────────────────────────── */

static pthread_mutex_t s_tap_mu = PTHREAD_MUTEX_INITIALIZER;
static void  *s_tap_ctx    = NULL;
static pthread_t s_tap_thread;

int ml_tap_start(ml_t h, int sample_rate, int chunk_ms,
                 ml_audio_chunk_fn on_chunk, void *ctx) {
    (void)h;
    if (!on_chunk) return -1;

    int fds[2];
    if (pipe(fds) != 0) return -1;

    void *tap = ml_tap_context_start(fds[1], sample_rate, NULL, NULL);
    if (!tap) { close(fds[0]); close(fds[1]); return -1; }

    PipeReader *pr = (PipeReader*)calloc(1, sizeof(PipeReader));
    pr->read_fd     = fds[0];
    pr->sample_rate = sample_rate;
    pr->chunk_ms    = chunk_ms;
    pr->on_chunk    = on_chunk;
    pr->chunk_ctx   = ctx;
    pr->running     = 1;

    pthread_mutex_lock(&s_tap_mu);
    s_tap_ctx = tap;
    pthread_mutex_unlock(&s_tap_mu);

    pthread_create(&s_tap_thread, NULL, pipe_reader_fn, pr);
    return 0;
}

void ml_tap_stop(ml_t h) {
    (void)h;
    pthread_mutex_lock(&s_tap_mu);
    void *tap = s_tap_ctx;
    s_tap_ctx = NULL;
    pthread_mutex_unlock(&s_tap_mu);
    if (tap) {
        ml_tap_context_stop(tap);
        pthread_join(s_tap_thread, NULL);
    }
}

/* ── ml_mic_start / ml_mic_stop ─────────────────────────────────────────── */

static pthread_mutex_t s_mic_mu = PTHREAD_MUTEX_INITIALIZER;
static void  *s_mic_ctx    = NULL;
static pthread_t s_mic_thread;

int ml_mic_start(ml_t h, int sample_rate, int chunk_ms,
                 ml_audio_chunk_fn on_chunk, void *ctx) {
    (void)h;
    if (!on_chunk) return -1;

    int fds[2];
    if (pipe(fds) != 0) return -1;

    void *mic = ml_mic_context_start(fds[1], sample_rate, NULL, NULL);
    if (!mic) { close(fds[0]); close(fds[1]); return -1; }

    PipeReader *pr = (PipeReader*)calloc(1, sizeof(PipeReader));
    pr->read_fd     = fds[0];
    pr->sample_rate = sample_rate;
    pr->chunk_ms    = chunk_ms;
    pr->on_chunk    = on_chunk;
    pr->chunk_ctx   = ctx;
    pr->running     = 1;

    pthread_mutex_lock(&s_mic_mu);
    s_mic_ctx = mic;
    pthread_mutex_unlock(&s_mic_mu);

    pthread_create(&s_mic_thread, NULL, pipe_reader_fn, pr);
    return 0;
}

void ml_mic_stop(ml_t h) {
    (void)h;
    pthread_mutex_lock(&s_mic_mu);
    void *mic = s_mic_ctx;
    s_mic_ctx = NULL;
    pthread_mutex_unlock(&s_mic_mu);
    if (mic) {
        ml_mic_context_stop(mic);
        pthread_join(s_mic_thread, NULL);
    }
}
