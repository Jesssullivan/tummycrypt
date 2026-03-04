import FileProvider
import Foundation

@main
struct TCFSProviderApp {
    static func main() {
        // Register the FileProvider domain so macOS activates our extension.
        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier("io.tinyland.tcfs"),
            displayName: "TCFS"
        )

        NSFileProviderManager.add(domain) { error in
            if let error = error {
                let nsError = error as NSError
                // -1004 = domain already exists (not an error)
                if nsError.domain == NSFileProviderErrorDomain && nsError.code == -1004 {
                    print("TCFS domain already registered")
                } else {
                    print("Failed to add domain: \(error)")
                }
            } else {
                print("TCFS FileProvider domain registered")
            }
        }

        // Keep running so the extension stays available.
        // LSUIElement = true in Info.plist prevents dock icon.
        RunLoop.current.run()
    }
}
