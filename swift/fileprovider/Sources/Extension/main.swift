import Foundation

// NSExtensionMain is a C function exported by Foundation that sets up
// the full extension hosting infrastructure:
//   1. Parses -LaunchArguments from ExtensionKit
//   2. Registers XPC listener for fileproviderd communication
//   3. Discovers NSExtensionPrincipalClass from Info.plist
//   4. Instantiates the principal class on first XPC connection
//   5. Runs the main dispatch loop (never returns)
//
// dispatchMain() alone does NOT set up XPC — it just blocks the main
// thread.  Without NSExtensionMain the extension process starts but
// fileproviderd gets Cocoa 4099 "connection … was invalidated".
@_silgen_name("NSExtensionMain")
func NSExtensionMain(_ argc: Int32, _ argv: UnsafeMutablePointer<UnsafeMutablePointer<Int8>?>) -> Int32

autoreleasepool {
    _ = NSExtensionMain(CommandLine.argc, CommandLine.unsafeArgv)
}
