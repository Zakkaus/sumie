// Test-only instrumentation linked into metal-spike. It swizzles CAMetalLayer and the concrete
// MTLCommandBuffer class to observe wgpu's real calls; it never replaces wgpu's surface logic.
#import <AppKit/AppKit.h>
#import <Metal/Metal.h>
#import <QuartzCore/CAMetalLayer.h>
#import <dispatch/dispatch.h>
#import <objc/runtime.h>

#include <mach/mach_time.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static pthread_mutex_t state = PTHREAD_MUTEX_INITIALIZER;
static bool resize;
static bool occluded;
static bool raced;
static atomic_ulong presented;
static NSUInteger attempt;
static NSUInteger gated;
static __weak CAMetalLayer *probeLayer;
static NSMutableArray *surfaces;
static id<MTLSharedEvent> gate;
static NSMutableArray<id<MTLCommandBuffer>> *gatedBuffers;
static id<CAMetalDrawable> (*originalAcquire)(CAMetalLayer *, SEL);
static void (*originalDevice)(CAMetalLayer *, SEL, id<MTLDevice>);
static void (*originalSize)(CAMetalLayer *, SEL, CGSize);
static void (*originalLabel)(id<MTLCommandBuffer>, SEL, NSString *);

static void emit(const char *format, ...) {
    char line[256];
    va_list args;
    va_start(args, format);
    int n = vsnprintf(line, sizeof line - 1, format, args);
    va_end(args);
    if (n < 0 || (size_t)n >= sizeof line - 1) abort();
    line[n++] = '\n';
    if (write(STDOUT_FILENO, line, (size_t)n) != n) abort();
}

static void require(bool condition, const char *reason) {
    if (!condition) {
        emit("NATIVE_ERROR %s", reason);
        abort();
    }
}

static IMP replace(Class cls, SEL selector, IMP replacement) {
    Method method = class_getInstanceMethod(cls, selector);
    require(method != NULL, "selector unavailable");
    IMP original = method_getImplementation(method);
    class_replaceMethod(cls, selector, replacement, method_getTypeEncoding(method));
    return original;
}

static NSWindow *hostingWindow(CAMetalLayer *layer) {
    for (CALayer *node = layer; node != nil; node = node.superlayer) {
        id delegate = node.delegate;
        if ([delegate isKindOfClass:NSView.class]) return [(NSView *)delegate window];
    }
    return nil;
}

static bool isProbeWindow(NSWindow *window) {
    return [window.title isEqualToString:@"sumie Metal spike"];
}

static bool isVisible(NSWindow *window) {
    return [window isVisible] && ![window isMiniaturized] &&
           (window.occlusionState & NSWindowOcclusionStateVisible) != 0;
}

static uint64_t micros(void) {
    static mach_timebase_info_data_t base;
    if (base.denom == 0) mach_timebase_info(&base);
    return mach_absolute_time() * base.numer / base.denom / 1000;
}

// Surface index 1..3 in first-seen order: shows whether an acquire reused a pool surface.
static NSUInteger surfaceIndex(id<CAMetalDrawable> drawable) {
    NSValue *key = [NSValue valueWithPointer:(const void *)drawable.texture.iosurface];
    NSUInteger found = [surfaces indexOfObject:key];
    if (found != NSNotFound) return found + 1;
    [surfaces addObject:key];
    return surfaces.count;
}

static id<CAMetalDrawable> acquire(CAMetalLayer *layer, SEL selector) {
    require([NSThread isMainThread], "nextDrawable not on main");
    NSWindow *window = hostingWindow(layer);
    require(isProbeWindow(window), "nextDrawable has no probe window");
    require(isVisible(window), "nextDrawable has no visible window");
    NSUInteger capacity = layer.maximumDrawableCount;
    require(capacity == 3, "unexpected layer capacity for frame latency 2");
    require(!layer.allowsNextDrawableTimeout, "wgpu did not disable drawable timeout");
    pthread_mutex_lock(&state);
    if (probeLayer == nil) probeLayer = layer;
    require(probeLayer == layer, "acquires did not use one layer");
    NSUInteger index = ++attempt;
    // Fields: attempt, capacity, GPU-gated presents still pending, visible.
    emit("ACQUIRE_ENTER %lu %lu %lu 1", index, capacity, (unsigned long)gatedBuffers.count);
    pthread_mutex_unlock(&state);

    uint64_t start = micros();
    id<CAMetalDrawable> drawable = originalAcquire(layer, selector);
    uint64_t waited = micros() - start;
    require(drawable != nil, "nextDrawable returned nil");
    require(isVisible(window), "window was not visible on return from nextDrawable");
    [drawable addPresentedHandler:^(id<MTLDrawable> shown) {
        (void)shown;
        atomic_fetch_add(&presented, 1);
    }];
    pthread_mutex_lock(&state);
    emit("ACQUIRE_RETURN %lu surface %lu us %llu", index, surfaceIndex(drawable),
         (unsigned long long)waited);
    pthread_mutex_unlock(&state);
    return drawable;
}

static void setLabel(id<MTLCommandBuffer> buffer, SEL selector, NSString *label) {
    originalLabel(buffer, selector, label);
    if (![label isEqualToString:@"sumie-metal-spike-gated"]) return;
    require([NSThread isMainThread], "gated command encoded off main");
    pthread_mutex_lock(&state);
    if (gate == nil) gate = [buffer.device newSharedEvent];
    require(gate != nil && gate.signaledValue == 0, "no unsignaled shared event");
    NSUInteger value = ++gated;
    [gatedBuffers addObject:buffer];
    // The pinned HAL labels the real submission buffer before recording any commands.
    // Each gated submission waits for its own event value, so the controller releases one at a time.
    [buffer encodeWaitForEvent:gate value:value];
    [buffer addCompletedHandler:^(id<MTLCommandBuffer> completed) {
        emit("GPU_DONE %lu %lu", (unsigned long)value, (unsigned long)completed.status);
    }];
    emit("GATE_ARMED %lu", (unsigned long)value);
    pthread_mutex_unlock(&state);
}

static void setDevice(CAMetalLayer *layer, SEL selector, id<MTLDevice> device) {
    originalDevice(layer, selector, device);
    if (device == nil || originalLabel != NULL || ![NSThread isMainThread] ||
        !isProbeWindow(hostingWindow(layer))) return;
    @autoreleasepool {
        // Discover the concrete class from the actual surface device. This sample is never committed.
        id<MTLCommandQueue> queue = [device newCommandQueueWithMaxCommandBufferCount:1];
        id<MTLCommandBuffer> sample = [queue commandBuffer];
        require(sample != nil, "cannot discover the Metal command buffer class");
        Class cls = object_getClass(sample);
        SEL label = @selector(setLabel:);
        Method method = class_getInstanceMethod(cls, label);
        require(method != NULL, "cannot instrument the Metal command buffer label");
        originalLabel = (void (*)(id<MTLCommandBuffer>, SEL, NSString *))method_getImplementation(method);
        class_replaceMethod(cls, label, (IMP)setLabel, method_getTypeEncoding(method));
    }
}

static void setSize(CAMetalLayer *layer, SEL selector, CGSize size) {
    originalSize(layer, selector, size);
    if ([NSThread isMainThread] && isProbeWindow(hostingWindow(layer))) {
        CGSize actual = layer.drawableSize;
        emit("LAYER_SIZE %.0f %.0f", actual.width, actual.height);
    }
}

// The occlusion race: wgpu sampled "visible" just before the window was covered. Keep acquiring
// and presenting as if that sample were stale, with Apple's drawable timeout enabled to bound
// each wait, until one acquire waits or RACE_FRAMES frames have been presented.
static void raceOcclusion(NSNotification *note) {
    NSWindow *window = note.object;
    CAMetalLayer *layer = probeLayer;
    if (raced || layer == nil || hostingWindow(layer) != window || isVisible(window)) return;
    raced = true;
    enum { RACE_FRAMES = 6 };
    id<MTLCommandQueue> queue = [layer.device newCommandQueue];
    layer.allowsNextDrawableTimeout = YES;
    for (unsigned long frame = 1; frame <= RACE_FRAMES; frame++) {
        unsigned long pending = (unsigned long)attempt + frame - 1 - atomic_load(&presented);
        uint64_t start = micros();
        id<CAMetalDrawable> drawable = originalAcquire(layer, @selector(nextDrawable));
        uint64_t waited = micros() - start;
        emit("RACE_ACQUIRE %lu us %llu %s pending %lu", frame, (unsigned long long)waited,
             drawable == nil ? "nil" : "drawable", pending);
        if (drawable == nil || waited >= 500000) break;
        [drawable addPresentedHandler:^(id<MTLDrawable> shown) {
            (void)shown;
            atomic_fetch_add(&presented, 1);
        }];
        id<MTLCommandBuffer> buffer = [queue commandBuffer];
        [buffer presentDrawable:drawable];
        [buffer commit];
        [buffer waitUntilCompleted];
    }
    layer.allowsNextDrawableTimeout = NO;
    emit("RACE_DONE");
}

// Runs on a GCD worker, never the main thread: signal the next gated submission.
static void releaseOne(void) {
    require(![NSThread isMainThread], "control release ran on main");
    pthread_mutex_lock(&state);
    require(gatedBuffers.count > 0, "no pending GPU gate");
    id<MTLCommandBuffer> buffer = gatedBuffers.firstObject;
    MTLCommandBufferStatus status = buffer.status;
    require(status == MTLCommandBufferStatusCommitted || status == MTLCommandBufferStatusScheduled,
            "gated command was not submitted and pending");
    uint64_t value = gate.signaledValue + 1;
    emit("GPU_RELEASE %llu %lu", (unsigned long long)value, (unsigned long)status);
    [gatedBuffers removeObjectAtIndex:0];
    gate.signaledValue = value;
    pthread_mutex_unlock(&state);
}

__attribute__((constructor)) static void install(void) {
    const char *mode = getenv("SUMIE_METAL_SCENARIO");
    const char *fd = getenv("SUMIE_METAL_RELEASE_FD");
    require(mode != NULL && fd != NULL, "launch with metal.py");
    resize = strstr(mode, "resize") != NULL;
    occluded = strcmp(mode, "occluded") == 0 || strcmp(mode, "minimized") == 0;
    if (occluded) {
        [NSNotificationCenter.defaultCenter addObserverForName:NSWindowDidChangeOcclusionStateNotification
                                                        object:nil
                                                         queue:NSOperationQueue.mainQueue
                                                    usingBlock:^(NSNotification *note) {
                                                        raceOcclusion(note);
                                                    }];
    }
    surfaces = [NSMutableArray array];
    gatedBuffers = [NSMutableArray array];
    originalAcquire = (id<CAMetalDrawable> (*)(CAMetalLayer *, SEL))
        replace(CAMetalLayer.class, @selector(nextDrawable), (IMP)acquire);
    originalDevice = (void (*)(CAMetalLayer *, SEL, id<MTLDevice>))
        replace(CAMetalLayer.class, @selector(setDevice:), (IMP)setDevice);
    if (resize) {
        originalSize = (void (*)(CAMetalLayer *, SEL, CGSize))
            replace(CAMetalLayer.class, @selector(setDrawableSize:), (IMP)setSize);
    }
    int control = atoi(fd);
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        char command;
        while (read(control, &command, 1) == 1) {
            require(command == 'R', "unknown watchdog command");
            @autoreleasepool {
                releaseOne();
            }
        }
        close(control);
    });
}
