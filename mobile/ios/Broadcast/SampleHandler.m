// The Broadcast Upload Extension. It is another process, so a frame cannot be a function call.
// Both sides map tinyclient-broadcast.bin in the app group. The 64-byte header is the same
// layout mediad's broadcast mailbox uses: magic, version, state, sequence, size, format,
// length, timestamp, then the pixels. The sequence goes odd, the pixels are copied, then it
// goes even. A reader copies only when both ends match.
//
// A late start does not overwrite a stop the app already asked for. When the app sets stop,
// the next sample ends the broadcast.

#import <ReplayKit/ReplayKit.h>
#import <CoreVideo/CoreVideo.h>
#include <stdatomic.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>

static const uint32_t kMagic = 0x544E5942u;
static const uint32_t kStateListening = 1u;
static const uint32_t kStateBroadcasting = 2u;
static const uint32_t kStateStop = 3u;
static const uint32_t kFormatBGRA = 1u;
static const uint32_t kFormatNV12 = 2u;
static const uint32_t kHeader = 64u;
static const uint32_t kMaxSide = 1280u;
static const size_t kMaxBytes = 1280u * 1280u * 4u;
static const size_t kFileLen = 64u + (1280u * 1280u * 4u);
static const OSType kBGRA = 0x42475241u;
static const OSType kFullRange = 0x34323066u;
static const OSType kVideoRange = 0x34323076u;
static NSString *const kGroup = @"group.dev.tinyclient.app";
static NSString *const kFile = @"tinyclient-broadcast.bin";

typedef struct {
    uint32_t magic;
    uint32_t version;
    _Atomic uint32_t state;
    _Atomic uint32_t seq;
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uint32_t data_len;
    _Atomic uint64_t timestamp_us;
    uint32_t reserved[6];
} TinyHeader;

@interface SampleHandler : RPBroadcastSampleHandler
@end

@implementation SampleHandler {
    TinyHeader *_header;
    uint8_t *_pixels;
    size_t _mapped;
}

- (void)broadcastStartedWithSetupInfo:(NSDictionary<NSString *, NSObject *> *)setupInfo {
    (void)setupInfo;
    NSError *error = nil;
    if (![self openMailbox:&error]) {
        [self finishBroadcastWithError:error];
    }
}

- (void)broadcastFinished {
    if (_header != NULL) {
        munmap(_header, _mapped);
        _header = NULL;
        _pixels = NULL;
    }
}

- (void)processSampleBuffer:(CMSampleBufferRef)sampleBuffer withType:(RPSampleBufferType)sampleBufferType {
    if (sampleBufferType != RPSampleBufferTypeVideo || _header == NULL) {
        return;
    }
    uint32_t state = atomic_load_explicit(&_header->state, memory_order_acquire);
    if (state == kStateStop) {
        [self finishBroadcastWithError:[self stopped]];
        return;
    }
    if (state != kStateListening && state != kStateBroadcasting) {
        return;
    }
    CVImageBufferRef image = CMSampleBufferGetImageBuffer(sampleBuffer);
    if (image == NULL) {
        return;
    }
    CVPixelBufferLockBaseAddress(image, kCVPixelBufferLock_ReadOnly);
    [self publish:image at:CMSampleBufferGetPresentationTimeStamp(sampleBuffer)];
    CVPixelBufferUnlockBaseAddress(image, kCVPixelBufferLock_ReadOnly);
}

- (BOOL)openMailbox:(NSError **)error {
    NSURL *base = [[NSFileManager defaultManager] containerURLForSecurityApplicationGroupIdentifier:kGroup];
    if (base == nil) {
        if (error != NULL) {
            *error = [self note:@"The screen recording group is not in the signature."];
        }
        return NO;
    }
    NSString *path = [[base URLByAppendingPathComponent:kFile] path];
    int fd = open(path.UTF8String, O_RDWR);
    if (fd < 0) {
        if (error != NULL) {
            *error = [self note:@"The screen share mailbox is not open."];
        }
        return NO;
    }
    struct stat info;
    int stated = fstat(fd, &info);
    if (stated != 0 || info.st_size < (off_t)kFileLen) {
        close(fd);
        if (error != NULL) {
            *error = [self note:@"The screen share mailbox is the wrong size."];
        }
        return NO;
    }
    void *ptr = mmap(NULL, kFileLen, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (ptr == MAP_FAILED) {
        if (error != NULL) {
            *error = [self note:@"The screen share mailbox could not be mapped."];
        }
        return NO;
    }
    TinyHeader *header = ptr;
    if (header->magic != kMagic) {
        munmap(ptr, kFileLen);
        if (error != NULL) {
            *error = [self note:@"Tiny is not waiting for a screen share."];
        }
        return NO;
    }
    uint32_t expected = kStateListening;
    bool swapped = atomic_compare_exchange_strong_explicit(&header->state, &expected, kStateBroadcasting, memory_order_acq_rel, memory_order_acquire);
    // Listening becomes broadcasting. Already broadcasting is a second start of the same share.
    // Stop, or a mailbox the app is not waiting on, ends the broadcast instead of writing over it.
    if (!swapped && expected != kStateBroadcasting) {
        munmap(ptr, kFileLen);
        if (error != NULL) {
            *error = [self stopped];
        }
        return NO;
    }
    _header = header;
    _pixels = (uint8_t *)ptr + kHeader;
    _mapped = kFileLen;
    return YES;
}

- (void)publish:(CVPixelBufferRef)image at:(CMTime)time {
    OSType format = CVPixelBufferGetPixelFormatType(image);
    size_t width = CVPixelBufferGetWidth(image);
    size_t height = CVPixelBufferGetHeight(image);
    size_t outW = 0;
    size_t outH = 0;
    [self fitWidth:width height:height outWidth:&outW outHeight:&outH];
    uint32_t kind = 0;
    size_t bytes = 0;
    if (format == kBGRA) {
        kind = kFormatBGRA;
        bytes = outW * outH * 4u;
        if (bytes > kMaxBytes) {
            return;
        }
        uint8_t *base = CVPixelBufferGetBaseAddress(image);
        size_t stride = CVPixelBufferGetBytesPerRow(image);
        if (base == NULL) {
            return;
        }
        for (size_t y = 0; y < outH; y++) {
            size_t sy = y * height / outH;
            uint8_t *dst = _pixels + y * outW * 4u;
            for (size_t x = 0; x < outW; x++) {
                size_t sx = x * width / outW;
                const uint8_t *src = base + sy * stride + sx * 4u;
                dst[0] = src[0];
                dst[1] = src[1];
                dst[2] = src[2];
                dst[3] = src[3];
                dst += 4;
            }
        }
    } else if (format == kFullRange || format == kVideoRange) {
        kind = kFormatNV12;
        bytes = outW * outH + outW * outH / 2u;
        if (bytes > kMaxBytes || CVPixelBufferGetPlaneCount(image) < 2) {
            return;
        }
        uint8_t *yBase = CVPixelBufferGetBaseAddressOfPlane(image, 0);
        uint8_t *uvBase = CVPixelBufferGetBaseAddressOfPlane(image, 1);
        size_t yStride = CVPixelBufferGetBytesPerRowOfPlane(image, 0);
        size_t uvStride = CVPixelBufferGetBytesPerRowOfPlane(image, 1);
        if (yBase == NULL || uvBase == NULL) {
            return;
        }
        uint8_t *dstY = _pixels;
        for (size_t y = 0; y < outH; y++) {
            size_t sy = y * height / outH;
            for (size_t x = 0; x < outW; x++) {
                size_t sx = x * width / outW;
                dstY[y * outW + x] = yBase[sy * yStride + sx];
            }
        }
        uint8_t *dstUV = _pixels + outW * outH;
        size_t chromaH = height / 2u;
        size_t chromaW = width / 2u;
        size_t outChromaH = outH / 2u;
        size_t outChromaW = outW / 2u;
        for (size_t y = 0; y < outChromaH; y++) {
            size_t sy = chromaH == 0 ? 0 : y * chromaH / outChromaH;
            for (size_t x = 0; x < outChromaW; x++) {
                size_t sx = chromaW == 0 ? 0 : x * chromaW / outChromaW;
                const uint8_t *src = uvBase + sy * uvStride + sx * 2u;
                dstUV[y * outW + x * 2u] = src[0];
                dstUV[y * outW + x * 2u + 1] = src[1];
            }
        }
    } else {
        return;
    }
    uint32_t seq = atomic_load_explicit(&_header->seq, memory_order_relaxed);
    atomic_store_explicit(&_header->seq, seq + 1u, memory_order_release);
    _header->width = (uint32_t)outW;
    _header->height = (uint32_t)outH;
    _header->format = kind;
    _header->data_len = (uint32_t)bytes;
    int64_t micros = (int64_t)(CMTimeGetSeconds(time) * 1000000.0);
    if (micros < 0) {
        micros = 0;
    }
    atomic_store_explicit(&_header->timestamp_us, (uint64_t)micros, memory_order_relaxed);
    atomic_store_explicit(&_header->seq, seq + 2u, memory_order_release);
}

- (void)fitWidth:(size_t)width height:(size_t)height outWidth:(size_t *)outW outHeight:(size_t *)outH {
    double scale = 1.0;
    if (width > kMaxSide || height > kMaxSide) {
        double widthScale = (double)kMaxSide / (double)width;
        double heightScale = (double)kMaxSide / (double)height;
        scale = widthScale < heightScale ? widthScale : heightScale;
    }
    size_t fittedW = ((size_t)((double)width * scale)) & ~1u;
    size_t fittedH = ((size_t)((double)height * scale)) & ~1u;
    if (fittedW < 2u) {
        fittedW = 2u;
    }
    if (fittedH < 2u) {
        fittedH = 2u;
    }
    *outW = fittedW;
    *outH = fittedH;
}

- (NSError *)note:(NSString *)text {
    return [NSError errorWithDomain:@"dev.tinyclient.broadcast" code:1 userInfo:@{NSLocalizedDescriptionKey: text}];
}

- (NSError *)stopped {
    return [self note:@"Tiny stopped the share."];
}

@end
