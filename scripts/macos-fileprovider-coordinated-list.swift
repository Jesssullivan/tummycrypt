#!/usr/bin/env swift
import FileProvider
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

struct ProbeError: Error, CustomStringConvertible {
    let description: String
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

guard CommandLine.arguments.count >= 2 && CommandLine.arguments.count <= 4 else {
    fail("usage: macos-fileprovider-coordinated-list.swift <root-path> [domain-identifier] [display-name]")
}

let rootURL = URL(fileURLWithPath: CommandLine.arguments[1])
let domainIdentifier = CommandLine.arguments.count >= 3 ? CommandLine.arguments[2] : "io.tinyland.tcfs"
let displayName = CommandLine.arguments.count >= 4 ? CommandLine.arguments[3] : "TCFS"
var failures: [String] = []

func coordinatedDirectoryEntries(at url: URL) throws -> [String] {
    let coordinator = NSFileCoordinator(filePresenter: nil)
    var coordinationError: NSError?
    var operationError: Error?
    var entries: [String] = []

    coordinator.coordinate(readingItemAt: url, options: [], error: &coordinationError) { coordinatedURL in
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
        throw ProbeError(description: "coordinated list failed: \(describe(coordinationError))")
    }

    if let operationError {
        throw ProbeError(description: "coordinated list operation failed: \(describe(operationError))")
    }

    return entries
}

func userVisibleRootURL(domainIdentifier: String, displayName: String) throws -> URL {
    let domain = NSFileProviderDomain(
        identifier: NSFileProviderDomainIdentifier(domainIdentifier),
        displayName: displayName
    )
    guard let manager = NSFileProviderManager(for: domain) else {
        throw ProbeError(description: "NSFileProviderManager unavailable for domain \(domainIdentifier)")
    }

    let sem = DispatchSemaphore(value: 0)
    var resolvedURL: URL?
    var resolvedError: Error?
    manager.getUserVisibleURL(for: .rootContainer) { url, error in
        resolvedURL = url
        resolvedError = error
        sem.signal()
    }

    if sem.wait(timeout: .now() + .seconds(10)) == .timedOut {
        throw ProbeError(description: "getUserVisibleURL timed out for domain \(domainIdentifier)")
    }

    if let resolvedError {
        throw ProbeError(description: "getUserVisibleURL failed: \(describe(resolvedError))")
    }

    guard let resolvedURL else {
        throw ProbeError(description: "getUserVisibleURL returned no URL for domain \(domainIdentifier)")
    }

    return resolvedURL
}

func printEntries(_ entries: [String]) -> Bool {
    guard !entries.isEmpty else {
        return false
    }
    for entry in entries {
        print(entry)
    }
    return true
}

do {
    if printEntries(try coordinatedDirectoryEntries(at: rootURL)) {
        exit(0)
    }
    failures.append("direct coordinated list returned no entries for \(rootURL.path)")
} catch {
    failures.append("direct path: \(error)")
}

do {
    let visibleURL = try userVisibleRootURL(
        domainIdentifier: domainIdentifier,
        displayName: displayName
    )
    if printEntries(try coordinatedDirectoryEntries(at: visibleURL)) {
        exit(0)
    }
    failures.append("user-visible URL returned no entries for \(visibleURL.path)")
} catch {
    failures.append("user-visible URL: \(error)")
}

fail(failures.joined(separator: "\n"))
