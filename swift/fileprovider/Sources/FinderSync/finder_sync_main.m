// Entry point for the Finder Sync extension.
// Calls NSExtensionMain() which handles XPC listener setup
// and principal class discovery from Info.plist.

#import <Foundation/Foundation.h>

FOUNDATION_EXTERN int NSExtensionMain(int argc, char * _Nonnull argv[_Nonnull]);

int main(int argc, char *argv[]) {
    @autoreleasepool {
        return NSExtensionMain(argc, argv);
    }
}
