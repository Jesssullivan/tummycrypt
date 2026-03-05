import FileProvider
import Foundation

@main
struct TCFSProviderApp {
    static func main() {
        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier("io.tinyland.tcfs"),
            displayName: "TCFS"
        )

        let args = CommandLine.arguments
        let shouldReset = args.contains("--reset")

        // Run domain setup on a background thread so the main RunLoop
        // can process XPC callbacks from fileproviderd.
        DispatchQueue.global(qos: .userInitiated).async {
            if shouldReset {
                let sem = DispatchSemaphore(value: 0)
                print("Removing domain...")
                NSFileProviderManager.remove(domain) { error in
                    if let error = error {
                        print("Remove: \(error.localizedDescription)")
                    } else {
                        print("Domain removed")
                    }
                    sem.signal()
                }
                sem.wait()
                Thread.sleep(forTimeInterval: 3.0)
            }

            let addSem = DispatchSemaphore(value: 0)
            NSFileProviderManager.add(domain) { error in
                if let error = error {
                    let nsError = error as NSError
                    if nsError.domain == NSFileProviderErrorDomain && nsError.code == -1004 {
                        print("TCFS domain already registered")
                    } else {
                        print("Failed to add domain: \(error)")
                    }
                } else {
                    print("TCFS FileProvider domain registered")
                }
                addSem.signal()
            }
            addSem.wait()

            // Signal re-enumeration after domain is ready
            if let manager = NSFileProviderManager(for: domain) {
                manager.signalEnumerator(for: .rootContainer) { error in
                    print("Signal root: \(error?.localizedDescription ?? "OK")")
                }
                manager.reimportItems(below: .rootContainer) { error in
                    print("Reimport: \(error?.localizedDescription ?? "OK")")
                }
            }
        }

        // Main RunLoop — processes XPC callbacks and keeps app alive.
        RunLoop.current.run()
    }
}
