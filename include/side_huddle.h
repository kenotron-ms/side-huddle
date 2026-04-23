/**
     * side-huddle C API
     *
     * Detect Teams / Zoom / Google Meet sessions locally and capture audio as WAV.
     *
     * Lifecycle:
     *   SideHuddleHandle h = side_huddle_new();
     *   side_huddle_on(h, my_callback, my_ctx);
     *   side_huddle_start(h);
     *   // ... events arrive on background threads ...
     *   side_huddle_stop(h);
     *   side_huddle_free(h);
     *
     * String fields inside SHEvent are valid ONLY for the duration of the callback.
     * Copy them if you need them beyond that point.
     */
    #ifndef SIDE_HUDDLE_H
    #define SIDE_HUDDLE_H

    #include <stdint.h>

    #ifdef __cplusplus
    extern "C" {
    #endif

    /* ── Opaque handle ────────────────────────────────────────────────────────── */

    typedef void* SideHuddleHandle;

    /* ── Enums ────────────────────────────────────────────────────────────────── */

    typedef enum SHEventKind {
        SH_PERMISSION_STATUS    = 0,
        SH_PERMISSIONS_GRANTED  = 1,
        SH_MEETING_DETECTED     = 2,
        SH_MEETING_UPDATED      = 3,
        SH_MEETING_ENDED        = 4,
        SH_RECORDING_STARTED    = 5,
        SH_RECORDING_ENDED      = 6,
        SH_RECORDING_READY      = 7,
        SH_CAPTURE_STATUS       = 8,
        SH_ERROR                = 9,
        SH_SPEAKER_CHANGED      = 10,
    } SHEventKind;

    typedef enum SHPermission {
        SH_PERMISSION_MICROPHONE     = 0,
        SH_PERMISSION_SCREEN_CAPTURE = 1,
        SH_PERMISSION_ACCESSIBILITY  = 2,
    } SHPermission;

    typedef enum SHPermissionStatus {
        SH_PERM_GRANTED       = 0,
        SH_PERM_NOT_REQUESTED = 1,
        SH_PERM_DENIED        = 2,
    } SHPermissionStatus;

    typedef enum SHCaptureKind {
        SH_CAPTURE_AUDIO = 0,
        SH_CAPTURE_VIDEO = 1,
    } SHCaptureKind;

    /* ── Event struct ─────────────────────────────────────────────────────────── */

    /**
     * Flat event structure.  Check `kind` first, then read the relevant fields.
     * Fields not applicable to a given kind are NULL or 0.
     *
     * All `const char*` fields are valid ONLY during the callback invocation.
     */
    typedef struct SHEvent {
        SHEventKind  kind;

        /* String fields */
        const char*  app;          /**< Meeting app name                                          */
        const char*  title;        /**< Window title (SH_MEETING_UPDATED only)                    */
        const char*  path;         /**< Mixed WAV file path (SH_RECORDING_READY)                  */
        const char*  others_path;  /**< Tap-only WAV path — other participants (SH_RECORDING_READY) */
        const char*  self_path;    /**< Mic-only WAV path — local user (SH_RECORDING_READY)       */
        const char*  message;      /**< Error description (SH_ERROR only)                         */
        const char*  participant;  /**< Tab-separated speaker names (SH_SPEAKER_CHANGED); "" = silence */

        /* SH_PERMISSION_STATUS fields */
        SHPermission       permission;
        SHPermissionStatus perm_status;

        /* SH_CAPTURE_STATUS fields */
        SHCaptureKind capture_kind;
        int           capturing;   /**< 1 = capturing, 0 = interrupted */
    } SHEvent;

    /* ── Callback ─────────────────────────────────────────────────────────────── */

    /**
     * Event callback.  Called on a background thread.
     * @param event    Pointer to event data (valid only during this call).
     * @param userdata Opaque pointer you passed to side_huddle_on().
     */
    typedef void (*SHEventCallback)(const SHEvent* event, void* userdata);

    /* ── API ──────────────────────────────────────────────────────────────────── */

    /** Create a new listener.  Free with side_huddle_free(). */
    SideHuddleHandle side_huddle_new(void);

    /** Free a listener.  Safe to call on NULL. */
    void side_huddle_free(SideHuddleHandle handle);

    /**
     * Register an event handler.  Multiple calls register multiple handlers;
     * all are invoked in registration order for every event.
     */
    void side_huddle_on(SideHuddleHandle handle, SHEventCallback callback, void* userdata);

    /** Automatically record every detected meeting. */
    void side_huddle_auto_record(SideHuddleHandle handle);

    /** Start recording the current meeting.
     *  Call from within a SH_MEETING_DETECTED callback to opt in. */
    void side_huddle_record(SideHuddleHandle handle);

    /** Stop the active recording without stopping the meeting monitor.
     *  No-op if no recording is active. RecordingEnded and RecordingReady
     *  events fire asynchronously after the WAV files are written. */
    void side_huddle_stop_recording(SideHuddleHandle handle);

    /** Set the PCM sample rate in Hz (default: 16000).  Call before side_huddle_start(). */
    void side_huddle_set_sample_rate(SideHuddleHandle handle, uint32_t hz);

    /** Set the WAV output directory (default: cwd).  Call before side_huddle_start(). */
    void side_huddle_set_output_dir(SideHuddleHandle handle, const char* dir);

    /** Start monitoring.  Returns 0 on success, -1 on failure. */
    int  side_huddle_start(SideHuddleHandle handle);

    /** Stop monitoring and any active recording. */
    void side_huddle_stop(SideHuddleHandle handle);

    /** Return the library version string (static; do not free). */
    const char* side_huddle_version(void);

    #ifdef __cplusplus
    } /* extern "C" */
    #endif

    #endif /* SIDE_HUDDLE_H */
