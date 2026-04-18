#include "../include/meetinglistener.h"
#include <string.h>

void ml_mix(const int16_t* a, const int16_t* b, int16_t* out, int n_frames) {
    for (int i = 0; i < n_frames; i++) {
        int32_t sum = (int32_t)a[i] + (int32_t)b[i];
        if (sum >  32767) sum =  32767;
        if (sum < -32768) sum = -32768;
        out[i] = (int16_t)sum;
    }
}

const char* ml_version(void) {
    return "0.1.0";
}
