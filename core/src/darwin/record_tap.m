/**
 * darwin/record_tap.m — System audio capture via CATapDescription (macOS 14.2+)
 *
 * Uses AudioHardwareCreateProcessTap to capture all system audio output
 * in mono, then resamples to the requested rate via AudioConverter.
 */

#import <AVFoundation/AVFoundation.h>
#import <CoreAudio/CATapDescription.h>
#include <AudioToolbox/AudioToolbox.h>
#include <CoreAudio/CoreAudio.h>
#include <CoreAudio/AudioHardwareTapping.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "../../include/meetinglistener.h"

/* ── TapContext — state for one active tap session ──────────────────────────── */

typedef struct {
    int                 pipeFd;          /* write end — Go/Python/etc reads from read end */
    volatile int        stopping;

    AudioObjectID       tapID;
    AudioObjectID       aggrDeviceID;
    AudioDeviceIOProcID procID;
    AudioConverterRef   converter;

    double              targetRate;
    int                 targetChannels;
    double              sourceRate;
    UInt32              sourceChannels;
    UInt32              sourceBytesPerFrame;

    void               *convInputData;
    UInt32              convInputFrames;
    UInt32              convInputByteSize;

    /* Callback path (alternative to pipe) */
    ml_audio_chunk_fn   on_chunk;
    void               *chunk_ctx;
    int16_t            *chunk_buf;
    int                 chunk_frames;
    int                 chunk_capacity;
} TapContext;

/* ── Helpers ────────────────────────────────────────────────────────────────── */

static void writeAll(int fd, const void *buf, size_t n) {
    const char *p = (const char*)buf;
    size_t done = 0;
    while (done < n) {
        ssize_t r = write(fd, p + done, n - done);
        if (r <= 0) break;
        done += r;
    }
}

static OSStatus converterInputProc(AudioConverterRef conv, UInt32 *ioPackets,
    AudioBufferList *ioData, AudioStreamPacketDescription **desc, void *ud)
{
    (void)conv; (void)desc;
    TapContext *ctx = (TapContext*)ud;
    if (!ctx->convInputData || ctx->convInputFrames == 0) {
        *ioPackets = 0;
        ioData->mBuffers[0].mData = NULL;
        ioData->mBuffers[0].mDataByteSize = 0;
        return noErr;
    }
    *ioPackets = ctx->convInputFrames;
    ioData->mBuffers[0].mData = ctx->convInputData;
    ioData->mBuffers[0].mDataByteSize = ctx->convInputByteSize;
    ctx->convInputData = NULL;
    ctx->convInputFrames = ctx->convInputByteSize = 0;
    return noErr;
}

static OSStatus audioIOProc(AudioObjectID dev, const AudioTimeStamp *now,
    const AudioBufferList *inputData, const AudioTimeStamp *inputTime,
    AudioBufferList *outputData, const AudioTimeStamp *outputTime, void *ud)
{
    (void)dev; (void)now; (void)inputTime; (void)outputData; (void)outputTime;
    TapContext *ctx = (TapContext*)ud;
    if (!ctx || ctx->stopping) return noErr;
    if (!inputData || inputData->mNumberBuffers == 0) return noErr;
    const AudioBuffer *src = &inputData->mBuffers[0];
    if (!src->mData || src->mDataByteSize == 0) return noErr;

    if (!ctx->converter) {
        if (ctx->pipeFd >= 0) writeAll(ctx->pipeFd, src->mData, src->mDataByteSize);
        return noErr;
    }

    ctx->convInputData      = src->mData;
    UInt32 bpf              = ctx->sourceBytesPerFrame ? ctx->sourceBytesPerFrame : ctx->sourceChannels * 4;
    ctx->convInputFrames    = (bpf > 0) ? (src->mDataByteSize / bpf) : 0;
    ctx->convInputByteSize  = src->mDataByteSize;
    if (ctx->convInputFrames == 0) return noErr;

    UInt32 outFrames = (UInt32)ceil((double)ctx->convInputFrames * ctx->targetRate / ctx->sourceRate) + 32;
    UInt32 outBytes  = outFrames * (UInt32)ctx->targetChannels * 2;
    int16_t *outBuf  = (int16_t*)malloc(outBytes);
    if (!outBuf) return noErr;

    AudioBufferList outBL = {
        .mNumberBuffers = 1,
        .mBuffers = {{ .mNumberChannels = (UInt32)ctx->targetChannels,
                       .mDataByteSize   = outBytes,
                       .mData           = outBuf }}
    };
    UInt32 actualFrames = outFrames;
    OSStatus st = AudioConverterFillComplexBuffer(ctx->converter, converterInputProc,
                                                  ctx, &actualFrames, &outBL, NULL);
    if (st == noErr && actualFrames > 0) {
        UInt32 bytes = actualFrames * (UInt32)ctx->targetChannels * 2;
        if (ctx->pipeFd >= 0) {
            writeAll(ctx->pipeFd, outBuf, bytes);
        } else if (ctx->on_chunk) {
            ctx->on_chunk(outBuf, (int)actualFrames, (int)ctx->targetRate, ctx->chunk_ctx);
        }
    }
    free(outBuf);
    return noErr;
}

/* ── Process tap setup ─────────────────────────────────────────────────────── */

static OSStatus createProcessTap(AudioObjectID *outTapID) {
    CATapDescription *desc = [[CATapDescription alloc]
        initMonoGlobalTapButExcludeProcesses:@[]];
    desc.name = @"meetinglistener-tap";
    desc.UUID = [NSUUID UUID];
    desc.privateTap = YES;
    desc.muteBehavior = CATapUnmuted;
    return AudioHardwareCreateProcessTap(desc, outTapID);
}

static int waitForDevice(AudioObjectID devID) {
    AudioObjectPropertyAddress addr = {kAudioDevicePropertyDeviceIsAlive,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    for (int i = 0; i < 20; i++) {
        UInt32 alive = 0, sz = sizeof(alive);
        if (AudioObjectGetPropertyData(devID, &addr, 0, NULL, &sz, &alive) == noErr && alive)
            return 1;
        usleep(100000);
    }
    return 0;
}

/* ── Public functions ──────────────────────────────────────────────────────── */

/**
 * Start the process tap. Returns an opaque TapContext* or NULL on failure.
 * `pipe_fd` is the write end of a pipe; pass -1 to use the callback instead.
 */
void* ml_tap_context_start(int pipe_fd, int sample_rate,
                            ml_audio_chunk_fn on_chunk, void *chunk_ctx) {
    TapContext *ctx = (TapContext*)calloc(1, sizeof(TapContext));
    if (!ctx) return NULL;
    ctx->pipeFd         = pipe_fd;
    ctx->targetRate     = (double)sample_rate;
    ctx->targetChannels = 1;
    ctx->on_chunk       = on_chunk;
    ctx->chunk_ctx      = chunk_ctx;

    if (createProcessTap(&ctx->tapID) != noErr) { free(ctx); return NULL; }

    /* Get tap UID */
    AudioObjectPropertyAddress ua = {kAudioTapPropertyUID,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    CFStringRef uid = NULL; UInt32 sz = sizeof(uid);
    if (AudioObjectGetPropertyData(ctx->tapID, &ua, 0, NULL, &sz, &uid) != noErr) {
        AudioHardwareDestroyProcessTap(ctx->tapID); free(ctx); return NULL;
    }
    char tapUID[256] = {0};
    CFStringGetCString(uid, tapUID, sizeof(tapUID), kCFStringEncodingUTF8);
    CFRelease(uid);

    /* Create aggregate device */
    NSString *uidStr = [NSString stringWithUTF8String:tapUID];
    NSDictionary *tapEntry = @{ @(kAudioSubTapUIDKey): uidStr };
    NSDictionary *desc = @{
        @(kAudioAggregateDeviceNameKey):         @"MeetingListenerAgg",
        @(kAudioAggregateDeviceUIDKey):          [@"com.meetinglistener.agg." stringByAppendingString:[[NSUUID UUID] UUIDString]],
        @(kAudioAggregateDeviceSubDeviceListKey):@[],
        @(kAudioAggregateDeviceTapListKey):      @[tapEntry],
        @(kAudioAggregateDeviceTapAutoStartKey): @NO,
        @(kAudioAggregateDeviceIsPrivateKey):    @YES,
        @(kAudioAggregateDeviceIsStackedKey):    @NO,
    };
    if (AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)desc, &ctx->aggrDeviceID) != noErr
        || !waitForDevice(ctx->aggrDeviceID)) {
        AudioHardwareDestroyProcessTap(ctx->tapID); free(ctx); return NULL;
    }

    /* Get tap format */
    AudioStreamBasicDescription srcASBD = {0};
    AudioObjectPropertyAddress fa = {kAudioTapPropertyFormat,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    sz = sizeof(srcASBD);
    AudioObjectGetPropertyData(ctx->tapID, &fa, 0, NULL, &sz, &srcASBD);
    ctx->sourceRate           = srcASBD.mSampleRate ? srcASBD.mSampleRate : 48000;
    ctx->sourceChannels       = srcASBD.mChannelsPerFrame ? srcASBD.mChannelsPerFrame : 2;
    ctx->sourceBytesPerFrame  = srcASBD.mBytesPerFrame ? srcASBD.mBytesPerFrame : ctx->sourceChannels * 4;

    /* Build target ASBD */
    AudioStreamBasicDescription dstASBD = {
        .mSampleRate       = ctx->targetRate,
        .mFormatID         = kAudioFormatLinearPCM,
        .mFormatFlags      = kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
        .mChannelsPerFrame = (UInt32)ctx->targetChannels,
        .mBitsPerChannel   = 16,
        .mBytesPerFrame    = (UInt32)ctx->targetChannels * 2,
        .mFramesPerPacket  = 1,
        .mBytesPerPacket   = (UInt32)ctx->targetChannels * 2,
    };
    if (ctx->sourceRate != ctx->targetRate || ctx->sourceChannels != 1
            || srcASBD.mBitsPerChannel != 16) {
        AudioConverterNew(&srcASBD, &dstASBD, &ctx->converter);
    }

    /* Register IOProc and start */
    if (AudioDeviceCreateIOProcID(ctx->aggrDeviceID, audioIOProc, ctx, &ctx->procID) != noErr
        || AudioDeviceStart(ctx->aggrDeviceID, ctx->procID) != noErr) {
        if (ctx->converter) AudioConverterDispose(ctx->converter);
        AudioHardwareDestroyAggregateDevice(ctx->aggrDeviceID);
        AudioHardwareDestroyProcessTap(ctx->tapID);
        free(ctx);
        return NULL;
    }
    return ctx;
}

void ml_tap_context_stop(void *handle) {
    if (!handle) return;
    TapContext *ctx = (TapContext*)handle;
    ctx->stopping = 1;
    if (ctx->aggrDeviceID && ctx->procID) {
        AudioDeviceStop(ctx->aggrDeviceID, ctx->procID);
        AudioDeviceDestroyIOProcID(ctx->aggrDeviceID, ctx->procID);
    }
    if (ctx->converter)   { AudioConverterDispose(ctx->converter); }
    if (ctx->aggrDeviceID){ AudioHardwareDestroyAggregateDevice(ctx->aggrDeviceID); }
    if (ctx->tapID)       { AudioHardwareDestroyProcessTap(ctx->tapID); }
    if (ctx->pipeFd >= 0) { close(ctx->pipeFd); }
    free(ctx);
}
