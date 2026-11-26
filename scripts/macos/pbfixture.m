// The pasteboard check's own access to its named board, independent of the code under test.
//
// usage: pbfixture set NAME          (the string from stdin)
//        pbfixture get NAME          (the string to stdout; exit 3 if the board holds none)
//        pbfixture clear NAME
//        pbfixture count NAME        (the change count; NAME `general` is the general board,
//                                     whose contents this tool never reads or writes)
//        pbfixture provide NAME MS   (the string from stdin, promised lazily: each request
//                                     prints `asked`, then waits MS milliseconds before it
//                                     provides the data, or forever for -1. Prints `ready`
//                                     once the promise is on the board; the check kills it.)
#import <AppKit/AppKit.h>

static NSString *const kType = @"public.utf8-plain-text";

// The owner API: a promise through `NSPasteboardItem` data providers returns nothing to the
// first read of each reader (seen on macOS 27.0), then the cached data.
@interface Provider : NSObject
@property(copy) NSString *text;
@property long long delay;
@end

@implementation Provider
- (void)pasteboard:(NSPasteboard *)board provideDataForType:(NSString *)type {
    printf("asked\n");
    fflush(stdout);
    if (self.delay < 0) {
        for (;;) {
            sleep(1000);
        }
    }
    usleep((useconds_t)(self.delay * 1000));
    [board setString:self.text forType:type];
}

@end

static NSString *readStdin(void) {
    NSData *data = [[NSFileHandle fileHandleWithStandardInput] readDataToEndOfFile];
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (!text) {
        fprintf(stderr, "pbfixture: stdin is not UTF-8\n");
        exit(2);
    }
    return text;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 3) {
            fprintf(stderr, "usage: pbfixture {set,get,clear,count,provide} NAME [MS]\n");
            return 2;
        }
        NSString *command = @(argv[1]);
        NSString *name = @(argv[2]);
        if ([command isEqualToString:@"count"]) {
            NSPasteboard *board = [name isEqualToString:@"general"] ? [NSPasteboard generalPasteboard]
                                                                    : [NSPasteboard pasteboardWithName:name];
            printf("%ld\n", (long)board.changeCount);
            return 0;
        }
        if ([name isEqualToString:@"general"]) {
            fprintf(stderr, "pbfixture: only the change count of the general board\n");
            return 2;
        }
        NSPasteboard *board = [NSPasteboard pasteboardWithName:name];
        if ([command isEqualToString:@"set"]) {
            [board clearContents];
            return [board setString:readStdin() forType:kType] ? 0 : 1;
        }
        if ([command isEqualToString:@"clear"]) {
            [board clearContents];
            return 0;
        }
        if ([command isEqualToString:@"get"]) {
            NSString *text = [board stringForType:kType];
            if (!text) {
                return 3;
            }
            NSData *data = [text dataUsingEncoding:NSUTF8StringEncoding];
            fwrite(data.bytes, 1, data.length, stdout);
            return 0;
        }
        if ([command isEqualToString:@"provide"] && argc == 4) {
            Provider *provider = [Provider new];
            provider.text = readStdin();
            provider.delay = atoll(argv[3]);
            [board declareTypes:@[ kType ] owner:provider];
            printf("ready\n");
            fflush(stdout);
            [[NSRunLoop currentRunLoop] run];
            return 0;
        }
        fprintf(stderr, "pbfixture: unknown command\n");
        return 2;
    }
}
