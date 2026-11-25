// Test-only hooks that frames.py injects into the terminal runner with DYLD_INSERT_LIBRARIES.
// They time every -[CAMetalLayer nextDrawable], can return nil drawables (what wgpu reports as
// a timeout), and can hold one wgpu submission on an unsignaled MTLSharedEvent. They observe
// wgpu's real calls; the surface logic stays wgpu's.
//
// Commands, one byte each on the descriptor in SUMIE_FRAME_HOOK_FD:
//   H  hold the next wgpu submission    R  release it    N  return nil from the next two acquires
// Lines on stdout (mach-time microseconds):
//   hook acquire START_US WAIT_US drawable|nil|timeout    hook gated    hook released
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <QuartzCore/CAMetalLayer.h>
#import <dispatch/dispatch.h>
#import <objc/runtime.h>

#include <mach/mach_time.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

enum { NIL_ACQUIRES = 2 };

static pthread_mutex_t state = PTHREAD_MUTEX_INITIALIZER;
static atomic_bool holdArmed;
static atomic_int nilLeft;
static id<MTLSharedEvent> gate;
static id<CAMetalDrawable> (*originalAcquire)(CAMetalLayer *, SEL);
static void (*originalLabel)(id<MTLCommandBuffer>, SEL, NSString *);

static void emit(const char *format, ...) {
    char line[160];
    va_list args;
    va_start(args, format);
    int n = vsnprintf(line, sizeof line - 1, format, args);
    va_end(args);
    if (n < 0 || (size_t)n >= sizeof line - 1) abort();
    line[n++] = '\n';
    // One write per line, so lines never interleave with the runner's own.
    if (write(STDOUT_FILENO, line, (size_t)n) != n) abort();
}

static void require(bool condition, const char *reason) {
    if (!condition) {
        emit("hook error %s", reason);
        abort();
    }
}

static uint64_t micros(void) {
    static mach_timebase_info_data_t base;
    if (base.denom == 0) mach_timebase_info(&base);
    return mach_absolute_time() * base.numer / base.denom / 1000;
}

static IMP replace(Class cls, SEL selector, IMP replacement) {
    Method method = class_getInstanceMethod(cls, selector);
    require(method != NULL, "selector unavailable");
    IMP original = method_getImplementation(method);
    class_replaceMethod(cls, selector, replacement, method_getTypeEncoding(method));
    return original;
}

static id<CAMetalDrawable> acquire(CAMetalLayer *layer, SEL selector) {
    require([NSThread isMainThread], "nextDrawable off the main thread");
    require(layer.allowsNextDrawableTimeout, "the drawable timeout is off");
    uint64_t start = micros();
    int left = atomic_load(&nilLeft);
    while (left > 0 && !atomic_compare_exchange_weak(&nilLeft, &left, left - 1)) {}
    if (left > 0) {
        emit("hook acquire %llu 0 nil", (unsigned long long)start);
        return nil;
    }
    id<CAMetalDrawable> drawable = originalAcquire(layer, selector);
    emit("hook acquire %llu %llu %s", (unsigned long long)start,
         (unsigned long long)(micros() - start), drawable == nil ? "timeout" : "drawable");
    return drawable;
}

// wgpu labels its last command buffer of each submission, the one whose completion its fence
// observes, before it encodes the fence signal and commits. A wait encoded here holds both.
static void setLabel(id<MTLCommandBuffer> buffer, SEL selector, NSString *label) {
    originalLabel(buffer, selector, label);
    if (![label isEqualToString:@"(wgpu internal) Signal"] || !atomic_exchange(&holdArmed, false)) {
        return;
    }
    pthread_mutex_lock(&state);
    require(gate == nil, "a submission is already held");
    gate = [buffer.device newSharedEvent];
    require(gate != nil, "no shared event");
    [buffer encodeWaitForEvent:gate value:1];
    pthread_mutex_unlock(&state);
    emit("hook gated");
}

static void releaseHold(void) {
    pthread_mutex_lock(&state);
    id<MTLSharedEvent> held = gate;
    gate = nil;
    pthread_mutex_unlock(&state);
    require(held != nil, "nothing is held");
    held.signaledValue = 1;
    emit("hook released");
}

__attribute__((constructor)) static void install(void) {
    const char *fd = getenv("SUMIE_FRAME_HOOK_FD");
    if (fd == NULL) return;
    originalAcquire = (id<CAMetalDrawable> (*)(CAMetalLayer *, SEL))
        replace(CAMetalLayer.class, @selector(nextDrawable), (IMP)acquire);
    @autoreleasepool {
        // The concrete command buffer class belongs to the GPU driver; find it from a sample.
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        id<MTLCommandBuffer> sample = [[device newCommandQueue] commandBuffer];
        require(sample != nil, "no Metal command buffer");
        originalLabel = (void (*)(id<MTLCommandBuffer>, SEL, NSString *))
            replace(object_getClass(sample), @selector(setLabel:), (IMP)setLabel);
    }
    int control = atoi(fd);
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        char command;
        while (read(control, &command, 1) == 1) {
            switch (command) {
            case 'H': atomic_store(&holdArmed, true); break;
            case 'R': releaseHold(); break;
            case 'N': atomic_store(&nilLeft, NIL_ACQUIRES); break;
            default: require(false, "unknown command");
            }
        }
        close(control);
    });
}
