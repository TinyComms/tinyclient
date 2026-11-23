import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let env = ProcessInfo.processInfo.environment
let output = URL(fileURLWithPath: env["E2E_OUT"]!).appendingPathComponent("steps.jsonl")
let start = Double(env["E2E_T0"]!)!
let app = AXUIElementCreateApplication(pid)

func step(_ name: String, _ ok: Bool) {
    var data = try! JSONSerialization.data(withJSONObject: ["t": Date().timeIntervalSince1970 - start, "step": name, "ok": ok])
    data.append(10)
    if !FileManager.default.fileExists(atPath: output.path) { try! data.write(to: output); return }
    let handle = try! FileHandle(forWritingTo: output)
    try! handle.seekToEnd()
    try! handle.write(contentsOf: data)
    try! handle.close()
}

func value(_ node: AXUIElement, _ attribute: String) -> AnyObject? {
    var result: CFTypeRef?
    guard AXUIElementCopyAttributeValue(node, attribute as CFString, &result) == .success else { return nil }
    return result
}

func find(_ node: AXUIElement, _ pattern: String, depth: Int = 0) -> AXUIElement? {
    for attribute in [kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute, kAXHelpAttribute] {
        if let words = value(node, attribute) as? String, words.range(of: pattern, options: .regularExpression) != nil {
            return node
        }
    }
    guard depth < 30 else { return nil }
    for child in value(node, kAXChildrenAttribute) as? [AXUIElement] ?? [] {
        if let found = find(child, pattern, depth: depth + 1) { return found }
    }
    return nil
}

struct Failure: Error { let detail: String }

func awaitNode(_ pattern: String) throws -> AXUIElement {
    let until = Date().addingTimeInterval(18)
    repeat {
        if let node = find(app, pattern) { return node }
        Thread.sleep(forTimeInterval: 0.25)
    } while Date() < until
    throw Failure(detail: "Accessible element missing: " + pattern)
}

func press(_ node: AXUIElement) throws {
    guard AXUIElementPerformAction(node, kAXPressAction as CFString) == .success else {
        throw Failure(detail: "Cannot activate accessible element")
    }
}

var current = "open the Telegram conversation using its accessible row"
do {
    guard AXIsProcessTrusted() else { throw Failure(detail: "Grant Accessibility to the lab driver") }
    try press(awaitNode("^Telegram(?:$| · )"))
    try press(awaitNode("^Ines Marchetti(?:$|,)"))
    step(current, true)
    current = "read the Spanish message with sender and time"
    let message = try awaitNode("^Ines Marchetti, .*\\d.*horario del ferry")
    _ = try awaitNode("^Message$")
    step(current, true)
    current = "activate Translate from the message menu"
    guard AXUIElementSetAttributeValue(message, kAXFocusedAttribute as CFString, kCFBooleanTrue) == .success else {
        throw Failure(detail: "The message row does not take focus")
    }
    Thread.sleep(forTimeInterval: 0.2)
    CGEvent(keyboardEventSource: nil, virtualKey: 36, keyDown: true)?.postToPid(pid)
    CGEvent(keyboardEventSource: nil, virtualKey: 36, keyDown: false)?.postToPid(pid)
    try press(awaitNode("^Translate$"))
    _ = try awaitNode("The ferry schedule changed")
    _ = try awaitNode("Translated from Spanish")
    step(current, true)
    current = "Show original removes the translation and preserves the message"
    try press(awaitNode("^Show original$"))
    _ = try awaitNode("^Ines Marchetti, .*horario del ferry")
    let until = Date().addingTimeInterval(5)
    while find(app, "The ferry schedule changed|^Show original$") != nil && Date() < until {
        Thread.sleep(forTimeInterval: 0.25)
    }
    if find(app, "The ferry schedule changed|^Show original$") != nil { throw Failure(detail: "Translation is still visible") }
    step(current, true)
} catch {
    step(current, false)
    FileHandle.standardError.write(Data("\(error)\n".utf8))
    exit(1)
}
