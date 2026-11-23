import Foundation
import UniformTypeIdentifiers

enum ShareFiles {
    static let group = "group.dev.tinyclient.app"
    static let maxFiles = 32
    static let maxBytes: Int64 = 1_073_741_824

    struct Manifest: Codable {
        var text = ""
        var files: [String] = []
    }

    static var inbox: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)?
            .appendingPathComponent("ShareInbox", isDirectory: true)
    }

    static var outbox: URL {
        URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
            .appendingPathComponent("Library/Application Support/tinyclient/outbox", isDirectory: true)
    }

    static func cleanName(_ name: String) -> String {
        let forbidden = CharacterSet.controlCharacters.union(CharacterSet(charactersIn: "/\\"))
        let clean = name.components(separatedBy: forbidden).joined(separator: "_").trimmingCharacters(in: .whitespacesAndNewlines)
        if clean.isEmpty || clean == "." || clean == ".." { return "Attachment" }
        if clean.utf8.count <= 220 { return clean }
        let extensionName = (clean as NSString).pathExtension
        let suffix = extensionName.utf8.count <= 12 && !extensionName.isEmpty ? "." + extensionName : ""
        var prefix = ""
        for character in clean {
            if prefix.utf8.count + String(character).utf8.count > 200 { break }
            prefix.append(character)
        }
        return (prefix.isEmpty ? "Attachment" : prefix) + suffix
    }

    static func copy(_ source: URL, name: String, into folder: URL, limit: Int64) throws -> URL {
        let manager = FileManager.default
        let values = try source.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard values.isRegularFile == true, let size = values.fileSize, Int64(size) <= limit else {
            throw CocoaError(.fileReadTooLarge)
        }
        try manager.createDirectory(at: folder, withIntermediateDirectories: true)
        let destination = folder.appendingPathComponent(cleanName(name))
        try manager.copyItem(at: source, to: destination)
        return destination
    }

    static func size(_ file: URL) -> Int64 {
        Int64((try? file.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
    }

    @MainActor
    static func providerFile(_ provider: NSItemProvider, type: String, into folder: URL, limit: Int64) async throws -> URL {
        let suggested = provider.suggestedName.map(cleanName)
        return try await withCheckedThrowingContinuation { continuation in
            provider.loadFileRepresentation(forTypeIdentifier: type) { url, error in
                guard let url else {
                    continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
                    return
                }
                // The item provider may remove its temporary file when this callback returns.
                continuation.resume(with: Result {
                    let fallback = UTType(type)?.preferredFilenameExtension
                    var name = suggested ?? url.lastPathComponent
                    if (name as NSString).pathExtension.isEmpty, let fallback { name += "." + fallback }
                    return try copy(url, name: name, into: folder, limit: limit)
                })
            }
        }
    }
}
