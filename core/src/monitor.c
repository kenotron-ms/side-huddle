/**
 * monitor.c — Cross-platform meeting monitor state machine
 *
 * Implements the two-timer detection logic:
 *   - 2s sustain timer: mic must stay active before MeetingStarted fires
 *   - 20s end-grace timer: mic must stay quiet before MeetingEnded fires
 *
 * Platform-specific polling (CoreAudio on macOS, WASAPI on Windows) is done
 * by calling ml_platform_poll() which returns the active meeting PID + app name.
 * Platforms implement ml_platform_poll() in their own detect.c files.
 *
 * Uses GCD (Grand Central Dispatch) for timers on Apple platforms,
 * POSIX threads + timerfd on Linux, Windows thread pool on Win32.
 */

#include "../include/meetinglistener.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <pthread.h>
#include <stdatomic.h>

/* ── Platform poll interface ────────────────────────────────────────────────
 * Each platform implements this: scan audio sessions, return PID + app name
 * of the first active meeting app. Returns 0 if nothing active.
 */
extern int ml_platform_poll(char app_out[64], char bundle_out[256]);

/* ── Timer abstraction ───────────────────────────────────────────────────── */

#ifdef __APPLE__
#  include <dispatch/dispatch.h>
typedef dispatch_source_t ml_timer_t;

static ml_timer_t ml_timer_once(double delay_secs, void (*fn)(void*), void *ctx) {
dispatch_source_t src = dispatch_source_create(
    DISPATCH_SOURCE_TYPE_TIMER, 0, 0,
    dispatch_get_global_queue(DISPATCH_QUEUE_PRIORITY_DEFAULT, 0));
uint64_t ns = (uint64_t)(delay_secs * 1e9);
dispatch_source_set_timer(src, dispatch_time(DISPATCH_TIME_NOW, ns), DISPATCH_TIME_FOREVER, 0);
dispatch_source_set_event_handler_f(src, (dispatch_function_t)fn);
dispatch_set_context(src, ctx);
dispatch_resume(src);
return src;
}

static void ml_timer_cancel(ml_timer_t t) {
if (t) { dispatch_source_cancel(t); dispatch_release(t); }
}
#else
/* Stub for non-Apple — replace with platform timer implementation */
typedef void* ml_timer_t;
static ml_timer_t ml_timer_once(double d, void (*fn)(void*), void *ctx) { (void)d;(void)fn;(void)ctx; return NULL; }
static void ml_timer_cancel(ml_timer_t t) { (void)t; }
#endif

/* ── Monitor struct ─────────────────────────────────────────────────────── */

#define ML_POLL_INTERVAL_US  300000   /* 300 ms */
#define ML_SUSTAIN_SECS      2.0
#define ML_END_GRACE_SECS    20.0
#define MAX_APP_LEN          64
#define MAX_BUNDLE_LEN       256

typedef struct {
pthread_mutex_t  mu;

/* State */
int              in_meeting;
char             current_app[MAX_APP_LEN];
int              current_pid;
atomic_int       start_once_fired;   /* 0 = not fired, 1 = fired */

/* Timers */
ml_timer_t       start_timer;        /* 2s sustain */
ml_timer_t       end_timer;          /* 20s end-grace */

/* Callbacks */
ml_meeting_start_fn on_start;
ml_meeting_end_fn   on_end;
ml_error_fn         on_error;
void*               cb_ctx;

/* Recording contexts (platform-specific, opaque) */
void*            tap_ctx;
void*            mic_ctx;

/* Poll thread */
pthread_t        poll_thread;
volatile int     poll_running;

} ml_monitor;

/* Forward declarations */
static void fire_meeting_ended(ml_monitor *m);
static void end_timer_cb(void *ctx);
static void start_timer_cb(void *ctx);

/* ── Helpers ─────────────────────────────────────────────────────────────── */

static void cancel_start_timer(ml_monitor *m) {
if (m->start_timer) {
    ml_timer_cancel(m->start_timer);
    m->start_timer = NULL;
    atomic_store(&m->start_once_fired, 0);
}
}

static void cancel_end_timer(ml_monitor *m) {
if (m->end_timer) {
    ml_timer_cancel(m->end_timer);
    m->end_timer = NULL;
}
}

/* ── MeetingEnded — shared path ─────────────────────────────────────────── */

static void fire_meeting_ended(ml_monitor *m) {
pthread_mutex_lock(&m->mu);
if (!m->in_meeting) { pthread_mutex_unlock(&m->mu); return; }

char app[MAX_APP_LEN];
strncpy(app, m->current_app, MAX_APP_LEN - 1);
m->in_meeting   = 0;
m->current_app[0] = '\0';
m->current_pid  = 0;
atomic_store(&m->start_once_fired, 0);
cancel_end_timer(m);
pthread_mutex_unlock(&m->mu);

if (m->on_end) m->on_end(app, m->cb_ctx);
}

/* ── Timer callbacks ─────────────────────────────────────────────────────── */

typedef struct { ml_monitor *m; char app[MAX_APP_LEN]; int pid; } StartCtx;

static void start_timer_cb(void *ctx) {
StartCtx *sc = (StartCtx*)ctx;
ml_monitor *m = sc->m;

pthread_mutex_lock(&m->mu);
m->start_timer = NULL;  /* already fired */
if (m->in_meeting) { pthread_mutex_unlock(&m->mu); free(sc); return; }

m->in_meeting = 1;
strncpy(m->current_app, sc->app, MAX_APP_LEN - 1);
m->current_pid = sc->pid;
pthread_mutex_unlock(&m->mu);

if (m->on_start) m->on_start(sc->app, sc->pid, m->cb_ctx);
free(sc);
}

static void end_timer_cb(void *ctx) {
ml_monitor *m = (ml_monitor*)ctx;
pthread_mutex_lock(&m->mu);
m->end_timer = NULL;
pthread_mutex_unlock(&m->mu);
fire_meeting_ended(m);
}

/* ── Poll thread ─────────────────────────────────────────────────────────── */

static void handle_pid(ml_monitor *m, int pid, const char *app) {
pthread_mutex_lock(&m->mu);

if (pid != 0) {
    /* Mic is active — cancel any pending end-grace timer */
    cancel_end_timer(m);

    if (!m->in_meeting && m->start_timer == NULL
            && !atomic_load(&m->start_once_fired)) {
        /* Arm the 2s sustain timer */
        StartCtx *sc = (StartCtx*)malloc(sizeof(StartCtx));
        sc->m = m;
        sc->pid = pid;
        strncpy(sc->app, app, MAX_APP_LEN - 1);
        sc->app[MAX_APP_LEN - 1] = '\0';
        m->start_timer = ml_timer_once(ML_SUSTAIN_SECS, start_timer_cb, sc);
    }
    pthread_mutex_unlock(&m->mu);
    return;
}

/* pid == 0: mic went quiet */
if (m->start_timer) {
    cancel_start_timer(m);
}

if (m->in_meeting && m->end_timer == NULL) {
    m->end_timer = ml_timer_once(ML_END_GRACE_SECS, end_timer_cb, m);
}

pthread_mutex_unlock(&m->mu);
}

static void* poll_thread_fn(void *arg) {
ml_monitor *m = (ml_monitor*)arg;
int last_pid = 0;

while (m->poll_running) {
    char app[MAX_APP_LEN]       = {0};
    char bundle[MAX_BUNDLE_LEN] = {0};
    int pid = ml_platform_poll(app, bundle);

    if (pid != last_pid) {
        last_pid = pid;
        handle_pid(m, pid, app);
    }

    /* Sleep 300ms between polls */
    struct timespec ts = {0, ML_POLL_INTERVAL_US * 1000};
    nanosleep(&ts, NULL);
}
return NULL;
}

/* ── Public API ─────────────────────────────────────────────────────────── */

ml_t ml_new(void) {
ml_monitor *m = (ml_monitor*)calloc(1, sizeof(ml_monitor));
if (!m) return NULL;
pthread_mutex_init(&m->mu, NULL);
atomic_init(&m->start_once_fired, 0);
return (ml_t)m;
}

void ml_free(ml_t h) {
if (!h) return;
ml_stop(h);
ml_monitor *m = (ml_monitor*)h;
pthread_mutex_destroy(&m->mu);
free(m);
}

int ml_start(ml_t h,
         ml_meeting_start_fn on_start,
         ml_meeting_end_fn   on_end,
         ml_error_fn         on_error,
         void*               ctx) {
if (!h) return -1;
ml_monitor *m = (ml_monitor*)h;
m->on_start = on_start;
m->on_end   = on_end;
m->on_error = on_error;
m->cb_ctx   = ctx;
m->poll_running = 1;
int rc = pthread_create(&m->poll_thread, NULL, poll_thread_fn, m);
if (rc != 0) { m->poll_running = 0; return rc; }
return 0;
}

void ml_stop(ml_t h) {
if (!h) return;
ml_monitor *m = (ml_monitor*)h;

m->poll_running = 0;
pthread_join(m->poll_thread, NULL);

pthread_mutex_lock(&m->mu);
cancel_start_timer(m);
cancel_end_timer(m);
pthread_mutex_unlock(&m->mu);

/* Stop any active recordings */
ml_tap_stop(h);
ml_mic_stop(h);
}
