import XCTest

/// Plays an action script against an app that is already running, then returns.
///
///   LAB_ACTIONS   "tap 0.5 0.3; wait 1.5; note opened a conversation; home"
///                 tap X Y   a tap at X,Y as fractions of the app's frame (0..1)
///                 wait S    seconds
///                 swipe X1 Y1 X2 Y2   a drag, as fractions
///                 note TEXT write a line to LAB_LOG now
///                 home      press the home button
///   LAB_LOG       file to append "<epoch seconds>\t<what>" lines to
///   LAB_BUNDLE    the app's bundle id (default dev.tinyclient.app)
final class LabActions: XCTestCase {
    func testActions() throws {
        let env = ProcessInfo.processInfo.environment
        let script = env["LAB_ACTIONS"] ?? ""
        let logPath = env["LAB_LOG"] ?? "/tmp/lab-actions.log"
        let app = XCUIApplication(bundleIdentifier: env["LAB_BUNDLE"] ?? "dev.tinyclient.app")

        func note(_ text: String) {
            let line = String(format: "%.3f\t%@\n", Date().timeIntervalSince1970, text)
            if let handle = FileHandle(forWritingAtPath: logPath) {
                handle.seekToEndOfFile(); handle.write(line.data(using: .utf8)!); try? handle.close()
            } else {
                try? line.write(toFile: logPath, atomically: false, encoding: .utf8)
            }
        }
        func point(_ x: String, _ y: String) -> XCUICoordinate {
            app.coordinate(withNormalizedOffset: CGVector(dx: Double(x) ?? 0.5, dy: Double(y) ?? 0.5))
        }

        note("runner started")
        for raw in script.split(separator: ";") {
            let words = raw.split(separator: " ").map(String.init)
            guard let verb = words.first else { continue }
            switch verb {
            case "tap" where words.count >= 3:
                point(words[1], words[2]).tap()
                note("tap \(words[1]) \(words[2])")
            case "swipe" where words.count >= 5:
                point(words[1], words[2]).press(forDuration: 0.05, thenDragTo: point(words[3], words[4]))
                note("swipe")
            case "wait" where words.count >= 2:
                Thread.sleep(forTimeInterval: Double(words[1]) ?? 1)
            case "home":
                XCUIDevice.shared.press(.home)
                note("home")
            case "note":
                note(words.dropFirst().joined(separator: " "))
            default:
                XCTFail("unknown action: \(raw)")
            }
        }
        note("runner done")
    }
}
