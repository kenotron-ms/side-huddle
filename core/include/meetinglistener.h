/**
     * meetinglistener.h — Public C ABI for libmeetinglistener
     *
     * Cross-platform meeting detection and audio capture library.
     * All callbacks fire on the library's internal thread — dispatch to your own
     * queue/thread if you need to do anything non-trivial.
     *
     * Quick start:
     *   ml_t h = ml_new();
     *   ml_start(h, on_start, on_end, on_error, userdata);
     *   ml_tap_start(h, 16000, on_audio, userdata);  // system audio (remote participants)
     *   ml_mic_start(h, 16000, on_audio, userdata);  // microphone  (local voice)
     *   // ... meeting happens ...
     *   ml_stop(h);
     *   ml_free(h);
     */
    #pragma once
    #ifdef __cplusplus
    extern "C" {
    #endif

    #include <stdint.h>

    /** Opaque handle. Create with ml_new(), destroy with ml_free(). */
    typedef void* ml_t;

    /* ── Callbacks ────────────────────────────────────────────────────────────────
     * Fired on the library's internal poll/timer thread.
     * Keep callbacks short — post to your own queue if you need more work.
     */

    /** Meeting started. `app` is e.g. "Microsoft Teams", "Zoom", "Google Meet". */
    typedef void (*ml_meeting_start_fn)(const char* app, int pid, void* ctx);

    /** Meeting ended. `app` is the same name passed to ml_meeting_start_fn. */
    typedef void (*ml_meeting_end_fn)  (const char* app,          void* ctx);

    /** Non-fatal error. Library keeps running. */
    typedef void (*ml_error_fn)        (const char* msg,          void* ctx);

    /**
     * PCM audio chunk. Called at approximately `chunk_duration_ms` intervals.
     *   pcm         — raw PCM-16 LE mono samples; valid only during this call
     *   n_frames    — number of samples
     *   sample_rate — samples per second (matches the value passed to ml_*_start)
     *   ctx         — userdata pointer from ml_tap_start / ml_mic_start
     */
    typedef void (*ml_audio_chunk_fn)(const int16_t* pcm, int n_frames,
                                      int sample_rate,    void* ctx);

    /* ── Lifecycle ────────────────────────────────────────────────────────────────
     */

    /** Allocate a new monitor. Returns NULL on allocation failure. */
    ml_t ml_new(void);

    /** Stop monitoring, stop any active recordings, and free all resources. */
    void ml_free(ml_t h);

    /* ── Detection ────────────────────────────────────────────────────────────────
     */

    /**
     * Start watching for meeting activity.
     * Polls every 300 ms (CoreAudio on macOS, WASAPI on Windows, PulseAudio on Linux).
     * MeetingStarted fires after 2 s of sustained mic input from a known meeting app.
     * MeetingEnded fires after 20 s of mic inactivity OR immediately when the call
     * window closes.
     *
     * Returns 0 on success, non-zero on error (details via on_error).
     */
    int ml_start(ml_t h,
                 ml_meeting_start_fn on_start,
                 ml_meeting_end_fn   on_end,
                 ml_error_fn         on_error,
                 void*               ctx);

    /** Stop detection and any active recordings. Safe to call multiple times. */
    void ml_stop(ml_t h);

    /* ── Recording ────────────────────────────────────────────────────────────────
     * Start either or both. Mix in your own code — see ml_mix() below.
     *
     * macOS tap requires macOS 14.2+ and Screen Recording permission.
     * Returns 0 on success.
     */

    /** System audio output tap — captures remote participants' voices. */
    int  ml_tap_start(ml_t h, int sample_rate, int chunk_ms,
                      ml_audio_chunk_fn on_chunk, void* ctx);
    void ml_tap_stop (ml_t h);

    /** Microphone — captures the local user's voice. */
    int  ml_mic_start(ml_t h, int sample_rate, int chunk_ms,
                      ml_audio_chunk_fn on_chunk, void* ctx);
    void ml_mic_stop (ml_t h);

    /* ── Utility ──────────────────────────────────────────────────────────────────
     */

    /**
     * Mix two equal-length PCM-16 mono buffers into `out`.
     * `out` must point to at least `n_frames * sizeof(int16_t)` bytes.
     * Samples are summed and clamped to [-32768, 32767].
     */
    void ml_mix(const int16_t* a, const int16_t* b, int16_t* out, int n_frames);

    /** Library version string, e.g. "0.1.0". */
    const char* ml_version(void);

    #ifdef __cplusplus
    }
    #endif
    