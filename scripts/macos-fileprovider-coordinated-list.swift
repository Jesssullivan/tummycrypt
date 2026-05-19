#!/usr/bin/env swift
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

func describe(_ error: Error) -> String {
    let nsError = error as NSError
    var details = "\(nsError.localizedDescription) [domain=\(nsError.domain) code=\(nsError.code)]"

    if let path = nsError.userInfo[NSFilePathErrorKey] as? String {
        details += " path=\(path)"
    }
    if let url = nsError.userInfo[NSURLErrorKey] as? URL {
        details += " url=\(url.path)"
    }
    if let underlying = nsError.userInfo[NSUnderlyingErrorKey] as? NSError {
        details += " underlying=\(underlying.domain)(\(underlying.code)): \(underlying.localizedDescription)"
    }

    return details
}

guard CommandLine.arguments.count == 2 else {
    fail("usage: macos-fileprovider-coordinated-list.swift <root-path>")
}

let rootURL = URL(fileURLWithPath: CommandLine.arguments[1])
let coordinator = NSFileCoordinator(filePresenter: nil)
var coordinationError: NSError?
var operationError: Error?
var entries: [String] = []

coordinator.coordinate(readingItemAt: rootURL, options: [], error: &coordinationError) { coordinatedURL in
    do {
        let accessed = coordinatedURL.startAccessingSecurityScopedResource()
        defer {
            if accessed {
                coordinatedURL.stopAccessingSecurityScopedResource()
            }
        }

        let children = try FileManager.default.contentsOfDirectory(
            at: coordinatedURL,
            includingPropertiesForKeys: nil,
            options: []
        )
        entries = children
            .map { $0.path }
            .sorted()
    } catch {
        operationError = error
    }
}

if let coordinationError {
    fail("coordinated list failed: \(describe(coordinationError))")
}

if let operationError {
    fail("coordinated list operation failed: \(describe(operationError))")
}

for entry in entries {
    print(entry)
}
