/* linux/detect.c — PulseAudio/PipeWire meeting detection stub */
#include "../../include/meetinglistener.h"
#include <string.h>

int ml_platform_poll(char app_out[64], char bundle_out[256]) {
    (void)app_out; (void)bundle_out;
    /* TODO: enumerate PulseAudio source-outputs, match to known meeting apps */
    return 0;
}

int  ml_tap_start(ml_t h, int sr, int ms, ml_audio_chunk_fn cb, void *ctx) { return -1; }
void ml_tap_stop (ml_t h) {}
int  ml_mic_start(ml_t h, int sr, int ms, ml_audio_chunk_fn cb, void *ctx) { return -1; }
void ml_mic_stop (ml_t h) {}
