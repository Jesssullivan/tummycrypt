import Foundation
import FileProvider

// FileProvider extension entry point.
// The system discovers TCFSFileProviderExtension via NSPrincipalClass in Info.plist.
autoreleasepool {
    // Keep the XPC service alive to handle FileProvider callbacks
    dispatchMain()
}
