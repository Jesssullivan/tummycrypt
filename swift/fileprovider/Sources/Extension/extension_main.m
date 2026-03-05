#import <Foundation/Foundation.h>

// Standard NSAppExtension entry point.
// Handles XPC listener setup, principal class discovery from
// NSExtensionPrincipalClass in Info.plist, and main dispatch loop.
FOUNDATION_EXTERN int NSExtensionMain(int argc, char * _Nonnull argv[_Nonnull]);

int main(int argc, char *argv[]) {
    @autoreleasepool {
        return NSExtensionMain(argc, argv);
    }
}
