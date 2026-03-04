import Foundation

@main
struct TCFSProviderApp {
    static func main() {
        // Minimal host app — exists only to contain the FileProvider extension.
        // LSUIElement = true in Info.plist prevents dock icon / menu bar.
        RunLoop.current.run()
    }
}
