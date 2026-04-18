/**
 * darwin/record_mic.m — Microphone capture via AVAudioEngine
 */

#import <AVFoundation/AVFoundation.h>
#include "../../include/meetinglistener.h"
#include <stdlib.h>
#include <unistd.h>
#include <math.h>

@interface MLMicCapture : NSObject
@property (nonatomic, strong) AVAudioEngine *engine;
@property (nonatomic) int pipeFd;
@property (nonatomic) int sampleRate;
@property (nonatomic) ml_audio_chunk_fn on_chunk;
@property (nonatomic) void *chunk_ctx;
@property (nonatomic) volatile int stopping;
- (instancetype)initWithPipe:(int)fd sampleRate:(int)sr
                    onChunk:(ml_audio_chunk_fn)cb ctx:(void*)ctx;
- (BOOL)start:(NSError**)err;
- (void)stop;
@end

@implementation MLMicCapture

- (instancetype)initWithPipe:(int)fd sampleRate:(int)sr
                    onChunk:(ml_audio_chunk_fn)cb ctx:(void*)ctx {
    self = [super init];
    if (!self) return nil;
    _engine     = [[AVAudioEngine alloc] init];
    _pipeFd     = fd;
    _sampleRate = sr;
    _on_chunk   = cb;
    _chunk_ctx  = ctx;
    return self;
}

- (BOOL)start:(NSError**)err {
    AVAudioInputNode *input = _engine.inputNode;
    AVAudioFormat *inputFmt = [input inputFormatForBus:0];
    AVAudioFormat *targetFmt = [[AVAudioFormat alloc]
        initWithCommonFormat:AVAudioPCMFormatInt16
                  sampleRate:(double)_sampleRate
                    channels:1
                 interleaved:YES];
    AVAudioConverter *conv = [[AVAudioConverter alloc] initFromFormat:inputFmt toFormat:targetFmt];

    __weak typeof(self) weakSelf = self;

    [input installTapOnBus:0 bufferSize:4096 format:inputFmt
                     block:^(AVAudioPCMBuffer *buf, AVAudioTime *when) {
        __strong typeof(weakSelf) self = weakSelf;
        if (!self || self.stopping) return;

        AVAudioFrameCount outCap = (AVAudioFrameCount)
            ceil((double)buf.frameLength * (double)self.sampleRate / inputFmt.sampleRate) + 32;
        AVAudioPCMBuffer *out = [[AVAudioPCMBuffer alloc]
            initWithPCMFormat:targetFmt frameCapacity:outCap];

        __block BOOL provided = NO;
        NSError *ce = nil;
        [conv convertToBuffer:out error:&ce withInputFromBlock:
            ^AVAudioBuffer*(AVAudioPacketCount n, AVAudioConverterInputStatus *s) {
                if (provided) { *s = AVAudioConverterInputStatus_NoDataNow; return nil; }
                provided = YES;
                *s = AVAudioConverterInputStatus_HaveData;
                return buf;
            }];

        if (ce || out.frameLength == 0) return;
        AudioBuffer ab = out.audioBufferList->mBuffers[0];
        if (!ab.mData || ab.mDataByteSize == 0) return;

        if (self.pipeFd >= 0) {
            const uint8_t *p = (const uint8_t*)ab.mData;
            size_t total = 0, n = ab.mDataByteSize;
            while (total < n) {
                ssize_t r = write(self.pipeFd, p + total, n - total);
                if (r <= 0) break;
                total += r;
            }
        } else if (self.on_chunk) {
            self.on_chunk((const int16_t*)ab.mData,
                          (int)out.frameLength,
                          self.sampleRate,
                          self.chunk_ctx);
        }
    }];

    [_engine prepare];
    return [_engine startAndReturnError:err];
}

- (void)stop {
    _stopping = 1;
    [_engine.inputNode removeTapOnBus:0];
    [_engine stop];
    if (_pipeFd >= 0) { close(_pipeFd); _pipeFd = -1; }
}
@end

/* ── C wrappers ─────────────────────────────────────────────────────────────── */

void* ml_mic_context_start(int pipe_fd, int sample_rate,
                            ml_audio_chunk_fn on_chunk, void *ctx) {
    MLMicCapture *cap = [[MLMicCapture alloc]
        initWithPipe:pipe_fd sampleRate:sample_rate onChunk:on_chunk ctx:ctx];
    NSError *err = nil;
    if (![cap start:&err]) {
        NSLog(@"ml_mic_context_start: %@", err);
        return NULL;
    }
    return (__bridge_retained void*)cap;
}

void ml_mic_context_stop(void *handle) {
    if (!handle) return;
    MLMicCapture *cap = (__bridge_transfer MLMicCapture*)handle;
    [cap stop];
}
