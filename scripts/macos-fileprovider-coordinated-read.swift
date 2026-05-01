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

guard CommandLine.arguments.count == 3 else {
    fail("usage: macos-fileprovider-coordinated-read.swift <source-path> <destination-path>")
}

let sourceURL = URL(fileURLWithPath: CommandLine.arguments[1])
let destinationURL = URL(fileURLWithPath: CommandLine.arguments[2])
let coordinator = NSFileCoordinator(filePresenter: nil)
var coordinationError: NSError?
var operationError: Error?

coordinator.coordinate(readingItemAt: sourceURL, options: [], error: &coordinationError) { coordinatedURL in
    do {
        let accessed = coordinatedURL.startAccessingSecurityScopedResource()
        defer {
            if accessed {
                coordinatedURL.stopAccessingSecurityScopedResource()
            }
        }

        let data = try Data(contentsOf: coordinatedURL)
        let destinationParent = destinationURL.deletingLastPathComponent()
        try FileManager.default.createDirectory(
            at: destinationParent,
            withIntermediateDirectories: true
        )
        try data.write(to: destinationURL, options: .atomic)
    } catch {
        operationError = error
    }
}

if let coordinationError {
    fail("coordinated read failed: \(describe(coordinationError))")
}

if let operationError {
    fail("coordinated read operation failed: \(describe(operationError))")
}
