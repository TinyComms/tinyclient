// TinyLabHelper: the one program the macOS lab uses to film and drive the tinyclient WINDOW.
//
// It exists so that nothing here can ever film the rest of the Mac: the recorder is handed a process
// id, asks ScreenCaptureKit for THAT process's window only (desktopIndependentWindow: the window's own
// pixels, wherever it sits and whatever covers it), and has no code path that captures a display.
// Input is posted to that process id only, never to the focused app.
//
// It must be started through LaunchServices (`open -n -a TinyLabHelper.app --args ...`), so macOS
// treats it as its own app and keeps its Screen Recording / Accessibility grants on it, instead of on
// sshd. Built by build.sh (swiftc, ad hoc signed); output goes to the file `open --stdout` names.
//
//   status                              screen recording + accessibility grants, one JSON line
//   windows --pid P                     the on-screen windows of P (id, x, y, w, h)
//   record  --pid-file F --out O.mp4 --ready R --done D [--fps 10] [--wait 120]
//                                       wait for pid F's first window, film it until SIGINT/SIGTERM.
//                                       Frame time is seconds since R was written (R holds the epoch).
//   key   --pid P --combo alt-down      post a key press (modifiers: cmd alt ctrl shift) to P only
//   click --pid P --x X --y Y           post a click at X,Y points inside P's first window
import AppKit
import AVFoundation
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

setvbuf(stdout, nil, _IOLBF, 0)
setvbuf(stderr, nil, _IOLBF, 0)

func say(_ text: String) { print(text) }
func die(_ text: String, _ code: Int32 = 1) -> Never {
    fputs("error: " + text + "\n", stderr)
    print("error: " + text)
    exit(code)
}

var args = Array(CommandLine.arguments.dropFirst())
// `open` may hand over the -psn_ marker on old systems; ignore anything that is not ours.
args.removeAll { $0.hasPrefix("-psn_") }
guard let command = args.first else { die("no command", 64) }
func option(_ name: String) -> String? {
    guard let at = args.firstIndex(of: "--" + name), at + 1 < args.count else { return nil }
    return args[at + 1]
}

struct Win {
    let id: CGWindowID
    let pid: pid_t
    let bounds: CGRect
}

/// On-screen windows of one process at layer 0 (ordinary windows), biggest first. Needs no grant.
func windows(of pid: pid_t) -> [Win] {
    let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    var found: [Win] = []
    for w in list {
        guard (w[kCGWindowOwnerPID as String] as? Int32) == pid, (w[kCGWindowLayer as String] as? Int) == 0,
              let id = w[kCGWindowNumber as String] as? UInt32,
              let dict = w[kCGWindowBounds as String] as? NSDictionary,
              let rect = CGRect(dictionaryRepresentation: dict) else { continue }
        if rect.width < 200 || rect.height < 200 { continue }
        found.append(Win(id: id, pid: pid, bounds: rect))
    }
    return found.sorted { $0.bounds.width * $0.bounds.height > $1.bounds.width * $1.bounds.height }
}

func status() {
    let screen = CGPreflightScreenCaptureAccess()
    let ax = AXIsProcessTrusted()
    say("{\"screen_recording\": \(screen), \"accessibility\": \(ax)}")
}

// MARK: input, to one process only

let keyCodes: [String: CGKeyCode] = [
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13,
    "e": 14, "r": 15, "y": 16, "t": 17, "k": 40, "return": 36, "enter": 36, "tab": 48, "space": 49, "escape": 53,
    "left": 123, "right": 124, "down": 125, "up": 126, "[": 33, "]": 30,
]

func post(key combo: String, to pid: pid_t) {
    var flags: CGEventFlags = []
    var name = ""
    for part in combo.split(separator: "-").map(String.init) {
        switch part {
        case "cmd", "secondary": flags.insert(.maskCommand)
        case "alt", "option": flags.insert(.maskAlternate)
        case "ctrl": flags.insert(.maskControl)
        case "shift": flags.insert(.maskShift)
        default: name = part
        }
    }
    guard let code = keyCodes[name] else { die("unknown key \(name)") }
    let source = CGEventSource(stateID: .privateState)
    for down in [true, false] {
        guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { die("no event") }
        event.flags = flags
        event.postToPid(pid)
        usleep(40_000)
    }
}

func post(clickAt point: CGPoint, to pid: pid_t) {
    let source = CGEventSource(stateID: .privateState)
    for (type, hold) in [(CGEventType.mouseMoved, 0), (.leftMouseDown, 60_000), (.leftMouseUp, 0)] {
        guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: point, mouseButton: .left) else { die("no event") }
        event.postToPid(pid)
        usleep(useconds_t(hold + 30_000))
    }
}

// MARK: recorder

final class Recorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let pidFile: String, out: String, ready: String, done: String
    let fps: Int, waitSeconds: Double
    var t0 = 0.0
    var stream: SCStream?
    var writer: AVAssetWriter?
    var input: AVAssetWriterInput?
    var adaptor: AVAssetWriterInputPixelBufferAdaptor?
    var last: CVPixelBuffer?
    var lastAt = 0.0
    var lastFrameTime = -1.0
    var width = 0, height = 0
    let queue = DispatchQueue(label: "lab.record")
    var stopping = false

    init(pidFile: String, out: String, ready: String, done: String, fps: Int, wait: Double) {
        self.pidFile = pidFile; self.out = out; self.ready = ready; self.done = done; self.fps = fps; self.waitSeconds = wait
    }

    func now() -> Double { Date().timeIntervalSince1970 }

    func start() async {
        guard CGPreflightScreenCaptureAccess() else {
            die("Screen Recording is not granted to TinyLabHelper (System Settings > Privacy & Security > Screen & System Audio Recording)", 77)
        }
        // The clock starts now; the lab reads the epoch from this file for E2E_T0.
        t0 = now()
        try? String(format: "%.6f\n", t0).write(toFile: ready + ".tmp", atomically: false, encoding: .utf8)
        try? FileManager.default.removeItem(atPath: ready)
        try? FileManager.default.moveItem(atPath: ready + ".tmp", toPath: ready)
        // Wait for the driver to name the app's process, then for that process's window.
        var pid: pid_t = 0
        var window: Win?
        let deadline = now() + waitSeconds
        while now() < deadline, !stopping {
            if pid == 0, let text = try? String(contentsOfFile: pidFile, encoding: .utf8), let p = Int32(text.trimmingCharacters(in: .whitespacesAndNewlines)) { pid = p }
            if pid != 0, let found = windows(of: pid).first { window = found; break }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        guard let target = window else {
            if stopping { finish(); return }
            die("no window for the app's process within \(Int(waitSeconds)) s", 78)
        }
        do {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
            // Match on the window id AND the process: nothing else can be selected.
            guard let scWindow = content.windows.first(where: { $0.windowID == target.id && $0.owningApplication?.processID == pid }) else {
                die("ScreenCaptureKit does not list window \(target.id) of pid \(pid)", 79)
            }
            let filter = SCContentFilter(desktopIndependentWindow: scWindow)
            let scale = Int(max(1, round(NSScreen.screens.first?.backingScaleFactor ?? 2)))
            let config = SCStreamConfiguration()
            // Even sizes for H.264; a point is `scale` pixels. Keep it near the size the lab can read.
            width = Int(scWindow.frame.width) * scale / 2 * 2
            height = Int(scWindow.frame.height) * scale / 2 * 2
            config.width = width
            config.height = height
            config.pixelFormat = kCVPixelFormatType_32BGRA
            config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
            config.showsCursor = false
            config.queueDepth = 5
            try openWriter()
            // Before the window was filmed, the picture is black: honest, and it keeps frame time = lab time.
            let s = SCStream(filter: filter, configuration: config, delegate: self)
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
            try await s.startCapture()
            stream = s
            say("recording window \(target.id) of pid \(pid) at \(width)x\(height)")
            queue.async { self.tick() }
        } catch {
            die("capture failed: \(error)", 80)
        }
    }

    func openWriter() throws {
        try? FileManager.default.removeItem(atPath: out)
        let w = try AVAssetWriter(outputURL: URL(fileURLWithPath: out), fileType: .mp4)
        let settings: [String: Any] = [
            AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: width, AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 4_000_000, AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel, AVVideoExpectedSourceFrameRateKey: fps],
        ]
        let i = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
        i.expectsMediaDataInRealTime = true
        let a = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: i, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: width, kCVPixelBufferHeightKey as String: height,
        ])
        w.add(i)
        guard w.startWriting() else { throw w.error ?? NSError(domain: "lab", code: 1) }
        w.startSession(atSourceTime: .zero)
        writer = w; input = i; adaptor = a
        // Black from lab time 0 to now, so the first real frame lands at its true time.
        var black: CVPixelBuffer?
        CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, nil, &black)
        if let black {
            CVPixelBufferLockBaseAddress(black, [])
            memset(CVPixelBufferGetBaseAddress(black), 0, CVPixelBufferGetDataSize(black))
            CVPixelBufferUnlockBaseAddress(black, [])
            var t = 0.0
            while t < now() - t0 { append(black, at: t); t += 1.0 / Double(fps) }
            last = black
        }
    }

    func append(_ buffer: CVPixelBuffer, at seconds: Double) {
        guard let input, let adaptor, input.isReadyForMoreMediaData, seconds > lastFrameTime else { return }
        if adaptor.append(buffer, withPresentationTime: CMTime(seconds: seconds, preferredTimescale: 1000)) { lastFrameTime = seconds }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = attachments.first?[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
              let pixels = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        last = pixels
        lastAt = now()
        append(pixels, at: lastAt - t0)
    }

    /// A still window sends no frames; repeat the last one so the file keeps the run's length.
    func tick() {
        if stopping { return }
        if let last, now() - t0 > lastFrameTime + 1.0 / Double(fps) { append(last, at: now() - t0) }
        queue.asyncAfter(deadline: .now() + 1.0 / Double(fps)) { self.tick() }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) { say("stream stopped: \(error)") }

    func stop() {
        stopping = true
        Task {
            try? await stream?.stopCapture()
            queue.sync { if let last { append(last, at: now() - t0) } }
            finish()
        }
    }

    func finish() {
        guard let writer else { exit(0) }
        input?.markAsFinished()
        writer.finishWriting {
            try? "done\n".write(toFile: self.done, atomically: true, encoding: .utf8)
            say("finished \(self.out): \(writer.status.rawValue)")
            exit(writer.status == .completed ? 0 : 1)
        }
    }
}

var sources: [DispatchSourceSignal] = []

switch command {
case "status":
    status()
case "windows":
    guard let p = option("pid").flatMap({ Int32($0) }) else { die("--pid", 64) }
    for w in windows(of: p) { say("{\"id\": \(w.id), \"x\": \(w.bounds.origin.x), \"y\": \(w.bounds.origin.y), \"w\": \(w.bounds.width), \"h\": \(w.bounds.height)}") }
case "key":
    guard let p = option("pid").flatMap({ Int32($0) }), let combo = option("combo") else { die("--pid --combo", 64) }
    guard AXIsProcessTrusted() else { die("Accessibility is not granted to TinyLabHelper", 77) }
    post(key: combo, to: p)
    say("ok key \(combo)")
case "click":
    guard let p = option("pid").flatMap({ Int32($0) }), let x = option("x").flatMap({ Double($0) }), let y = option("y").flatMap({ Double($0) }) else { die("--pid --x --y", 64) }
    guard AXIsProcessTrusted() else { die("Accessibility is not granted to TinyLabHelper", 77) }
    guard let w = windows(of: p).first else { die("the app has no window") }
    post(clickAt: CGPoint(x: w.bounds.origin.x + x, y: w.bounds.origin.y + y), to: p)
    say("ok click")
case "record":
    guard let pidFile = option("pid-file"), let out = option("out"), let ready = option("ready"), let done = option("done") else { die("--pid-file --out --ready --done", 64) }
    let recorder = Recorder(pidFile: pidFile, out: out, ready: ready, done: done, fps: option("fps").flatMap { Int($0) } ?? 10, wait: option("wait").flatMap { Double($0) } ?? 120)
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    for sig in [SIGINT, SIGTERM] {
        signal(sig, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
        source.setEventHandler { recorder.stop() }
        source.resume()
        sources.append(source)
    }
    Task { await recorder.start() }
    app.run()
default:
    die("unknown command \(command)", 64)
}

