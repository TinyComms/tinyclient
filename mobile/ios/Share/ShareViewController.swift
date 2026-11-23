import Social
import UIKit
import UniformTypeIdentifiers

final class ShareViewController: SLComposeServiceViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Save to Tiny"
        placeholder = "Add a note, then open Tiny to choose a conversation"
    }

    override func isContentValid() -> Bool { true }

    override func didSelectPost() {
        let providers = (extensionContext?.inputItems as? [NSExtensionItem] ?? []).flatMap { $0.attachments ?? [] }
        let note = contentText ?? ""
        Task {
            do {
                try await save(providers: providers, note: note)
                extensionContext?.completeRequest(returningItems: nil)
            } catch {
                extensionContext?.cancelRequest(withError: error)
            }
        }
    }

    override func configurationItems() -> [Any]! { [] }

    private func save(providers: [NSItemProvider], note: String) async throws {
        guard let inbox = ShareFiles.inbox, providers.count <= ShareFiles.maxFiles else {
            throw CocoaError(.fileWriteUnknown)
        }
        let id = UUID().uuidString
        let pending = inbox.appendingPathComponent("." + id, isDirectory: true)
        let ready = inbox.appendingPathComponent(id, isDirectory: true)
        let manager = FileManager.default
        try manager.createDirectory(at: pending, withIntermediateDirectories: true)
        do {
            var contents = ShareFiles.Manifest(text: note)
            var bytes = ShareFiles.maxBytes
            for (index, provider) in providers.enumerated() {
                if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
                    let source = try await fileURL(provider)
                    let access = source.startAccessingSecurityScopedResource()
                    defer { if access { source.stopAccessingSecurityScopedResource() } }
                    let file = try ShareFiles.copy(source, name: source.lastPathComponent,
                        into: pending.appendingPathComponent(String(index)), limit: bytes)
                    bytes -= ShareFiles.size(file)
                    contents.files.append("\(index)/\(file.lastPathComponent)")
                } else if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                    append(try await text(provider, type: .url), to: &contents.text)
                } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
                    append(try await text(provider, type: .plainText), to: &contents.text)
                } else if let type = provider.registeredTypeIdentifiers.first {
                    let file = try await ShareFiles.providerFile(provider, type: type,
                        into: pending.appendingPathComponent(String(index)), limit: bytes)
                    bytes -= ShareFiles.size(file)
                    contents.files.append("\(index)/\(file.lastPathComponent)")
                }
            }
            guard !contents.files.isEmpty || !contents.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                throw CocoaError(.fileReadUnknown)
            }
            try JSONEncoder().encode(contents).write(to: pending.appendingPathComponent("manifest.json"), options: .atomic)
            try manager.moveItem(at: pending, to: ready)
        } catch {
            try? manager.removeItem(at: pending)
            throw error
        }
    }

    private func append(_ value: String, to target: inout String) {
        guard !value.isEmpty, value != target else { return }
        target += (target.isEmpty ? "" : "\n") + value
    }

    private func text(_ provider: NSItemProvider, type: UTType) async throws -> String {
        try await withCheckedThrowingContinuation { continuation in
            provider.loadItem(forTypeIdentifier: type.identifier) { item, error in
                if let url = item as? URL { continuation.resume(returning: url.absoluteString) }
                else if let text = item as? String { continuation.resume(returning: text) }
                else if let error { continuation.resume(throwing: error) }
                else { continuation.resume(returning: "") }
            }
        }
    }

    private func fileURL(_ provider: NSItemProvider) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier) { item, error in
                if let url = item as? URL, url.isFileURL { continuation.resume(returning: url) }
                else { continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown)) }
            }
        }
    }
}
