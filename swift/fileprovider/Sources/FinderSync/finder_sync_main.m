// Entry point for the Finder Sync extension.
// FinderSync extensions are AppKit-based plugins that run inside Finder's process.
// They use NSApplicationMain (not NSExtensionMain which is for XPC services).

#import <Cocoa/Cocoa.h>

int main(int argc, const char * argv[]) {
    @autoreleasepool {
        return NSApplicationMain(argc, argv);
    }
}
