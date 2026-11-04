// Reads the text in a screenshot (Vision), one line per recognised string:
//   <x> <y> <w> <h> <text>      the box as fractions of the picture, y from the TOP
// Used by the iOS driver to assert what the app really shows, and to find where to tap. No grant needed.
import AppKit
import Vision

guard CommandLine.arguments.count > 1, let image = NSImage(contentsOfFile: CommandLine.arguments[1]),
      let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    fputs("usage: ocr picture.png\n", stderr); exit(64)
}
let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = false
try VNImageRequestHandler(cgImage: cg).perform([request])
for case let line as VNRecognizedTextObservation in request.results ?? [] {
    guard let best = line.topCandidates(1).first else { continue }
    let b = line.boundingBox
    print(String(format: "%.3f %.3f %.3f %.3f ", b.minX, 1 - b.maxY, b.width, b.height) + best.string)
}
