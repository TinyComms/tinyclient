import PhotosUI
import UIKit
import UniformTypeIdentifiers

private let attachmentIO = DispatchQueue(label: "dev.tinyclient.attachments")

private func deliver(_ contents: ShareFiles.Manifest, kind: Int32) {
    let json = (try? JSONEncoder().encode(contents)).map { String(decoding: $0, as: UTF8.self) } ?? "{\"error\":true}"
    json.withCString { tinyclient_native_share_received(kind, $0) }
}

private func failure(_ kind: Int32) {
    "{\"error\":true}".withCString { tinyclient_native_share_received(kind, $0) }
}

@MainActor
final class Attachments: NSObject, PHPickerViewControllerDelegate, UIImagePickerControllerDelegate,
    UINavigationControllerDelegate, UIDocumentPickerDelegate, UIAdaptivePresentationControllerDelegate {
    static let shared = Attachments()

    func install() {
        NotificationCenter.default.addObserver(self, selector: #selector(sceneActive), name: UIScene.didActivateNotification, object: nil)
    }

    @objc private func sceneActive() {
        attachmentIO.async {
            guard let inbox = ShareFiles.inbox else { return }
            let manager = FileManager.default
            let batches = (try? manager.contentsOfDirectory(at: inbox, includingPropertiesForKeys: nil)) ?? []
            for batch in batches where UUID(uuidString: batch.lastPathComponent) != nil {
                var staged: [URL] = []
                do {
                    let data = try Data(contentsOf: batch.appendingPathComponent("manifest.json"))
                    let manifest = try JSONDecoder().decode(ShareFiles.Manifest.self, from: data)
                    guard manifest.files.count <= ShareFiles.maxFiles else { throw CocoaError(.fileReadTooLarge) }
                    var remaining = ShareFiles.maxBytes
                    for relative in manifest.files {
                        let parts = relative.split(separator: "/", omittingEmptySubsequences: false)
                        guard parts.count == 2, Int(parts[0]) != nil, parts[1] == ShareFiles.cleanName(String(parts[1])) else {
                            throw CocoaError(.fileReadCorruptFile)
                        }
                        let source = batch.appendingPathComponent(relative)
                        let destination = try ShareFiles.copy(source, name: String(parts[1]), into: stagingFolder(), limit: remaining)
                        remaining -= ShareFiles.size(destination)
                        staged.append(destination)
                    }
                    try manager.removeItem(at: batch)
                    deliver(.init(text: manifest.text, files: staged.map(\.path)), kind: 1)
                } catch {
                    for path in staged { try? manager.removeItem(at: path) }
                    try? manager.removeItem(at: batch)
                    failure(1)
                }
            }
        }
    }

    private var presenter: UIViewController? {
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first { $0.activationState == .foregroundActive }
        var controller = scene?.keyWindow?.rootViewController
        while let child = controller?.presentedViewController, !(child is UIAlertController), !child.isBeingDismissed {
            controller = child
        }
        return controller
    }

    func choose() {
        guard let presenter else { failure(0); return }
        let alert = UIAlertController(title: "Attach", message: nil, preferredStyle: .actionSheet)
        alert.addAction(UIAlertAction(title: "Photos and videos", style: .default) { _ in self.photos() })
        if UIImagePickerController.isSourceTypeAvailable(.camera) {
            alert.addAction(UIAlertAction(title: "Take photo or video", style: .default) { _ in self.camera() })
        }
        alert.addAction(UIAlertAction(title: "Documents", style: .default) { _ in self.documents() })
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { _ in deliver(.init(), kind: 0) })
        anchor(alert, on: presenter)
        presenter.present(alert, animated: true)
        alert.presentationController?.delegate = self
    }

    private func anchor(_ controller: UIViewController, on presenter: UIViewController) {
        controller.popoverPresentationController?.sourceView = presenter.view
        let center = presenter.view.bounds
        controller.popoverPresentationController?.sourceRect = CGRect(x: center.midX, y: center.midY, width: 1, height: 1)
    }

    func share(_ file: URL) {
        guard let presenter else {
            try? FileManager.default.removeItem(at: file.deletingLastPathComponent())
            return
        }
        let activity = UIActivityViewController(activityItems: [file], applicationActivities: nil)
        activity.completionWithItemsHandler = { _, _, _, _ in
            attachmentIO.async { try? FileManager.default.removeItem(at: file.deletingLastPathComponent()) }
        }
        anchor(activity, on: presenter)
        presenter.present(activity, animated: true)
    }

    private func show(_ controller: UIViewController) {
        guard let presenter else { failure(0); return }
        presenter.present(controller, animated: true)
        controller.presentationController?.delegate = self
    }

    private func photos() {
        var configuration = PHPickerConfiguration()
        configuration.filter = .any(of: [.images, .videos])
        configuration.selectionLimit = ShareFiles.maxFiles
        configuration.preferredAssetRepresentationMode = .current
        let picker = PHPickerViewController(configuration: configuration)
        picker.delegate = self
        show(picker)
    }

    private func camera() {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        let available = UIImagePickerController.availableMediaTypes(for: .camera) ?? []
        picker.mediaTypes = [UTType.image.identifier, UTType.movie.identifier].filter { available.contains($0) }
        if picker.mediaTypes.isEmpty { failure(0); return }
        picker.delegate = self
        picker.modalPresentationStyle = .fullScreen
        show(picker)
    }

    private func documents() {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.item], asCopy: true)
        picker.allowsMultipleSelection = true
        picker.delegate = self
        show(picker)
    }

    func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
        picker.dismiss(animated: true)
        Task {
            var staged: [URL] = []
            var remaining = ShareFiles.maxBytes
            do {
                for result in results {
                    let provider = result.itemProvider
                    guard let type = provider.registeredTypeIdentifiers.first(where: {
                        guard let item = UTType($0) else { return false }
                        return item.conforms(to: .image) || item.conforms(to: .movie)
                    }) else { throw CocoaError(.fileReadUnknown) }
                    let destination = try await ShareFiles.providerFile(provider, type: type, into: stagingFolder(), limit: remaining)
                    remaining -= ShareFiles.size(destination)
                    staged.append(destination)
                }
                deliver(.init(files: staged.map(\.path)), kind: 0)
            } catch {
                for path in staged { try? FileManager.default.removeItem(at: path) }
                failure(0)
            }
        }
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        stage(urls)
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        deliver(.init(), kind: 0)
    }

    func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
        picker.dismiss(animated: true)
        deliver(.init(), kind: 0)
    }

    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        deliver(.init(), kind: 0)
    }

    func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
        picker.dismiss(animated: true)
        if let video = info[.mediaURL] as? URL {
            stage([video])
        } else if let photo = info[.originalImage] as? UIImage, let bytes = photo.jpegData(compressionQuality: 0.92) {
            attachmentIO.async {
                do {
                    let folder = stagingFolder()
                    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
                    let destination = folder.appendingPathComponent("Photo.jpg")
                    try bytes.write(to: destination, options: .atomic)
                    deliver(.init(files: [destination.path]), kind: 0)
                } catch { failure(0) }
            }
        } else {
            failure(0)
        }
    }

    private func stage(_ urls: [URL]) {
        attachmentIO.async {
            var staged: [URL] = []
            do {
                guard urls.count <= ShareFiles.maxFiles else { throw CocoaError(.fileReadTooLarge) }
                var remaining = ShareFiles.maxBytes
                for source in urls {
                    let access = source.startAccessingSecurityScopedResource()
                    defer { if access { source.stopAccessingSecurityScopedResource() } }
                    let destination = try ShareFiles.copy(source, name: source.lastPathComponent, into: stagingFolder(), limit: remaining)
                    remaining -= ShareFiles.size(destination)
                    staged.append(destination)
                }
                deliver(.init(files: staged.map(\.path)), kind: 0)
            } catch {
                for path in staged { try? FileManager.default.removeItem(at: path) }
                failure(0)
            }
        }
    }
}

private func stagingFolder() -> URL {
    ShareFiles.outbox.appendingPathComponent("phone-" + UUID().uuidString, isDirectory: true)
}

@_cdecl("tinyclient_native_pick_attachments")
func tinyclient_native_pick_attachments() {
    DispatchQueue.main.async { Attachments.shared.choose() }
}

@_cdecl("tinyclient_native_share_file")
func tinyclient_native_share_file(_ source: UnsafePointer<CChar>, _ name: UnsafePointer<CChar>) {
    let path = String(cString: source)
    let fileName = String(cString: name)
    attachmentIO.async {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("tiny-share-" + UUID().uuidString)
        do {
            let file = try ShareFiles.copy(URL(fileURLWithPath: path), name: fileName, into: folder, limit: ShareFiles.maxBytes)
            DispatchQueue.main.async { Attachments.shared.share(file) }
        } catch {
            try? FileManager.default.removeItem(at: folder)
        }
    }
}
