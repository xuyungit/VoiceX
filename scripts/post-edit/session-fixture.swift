// Controlled synthetic fields. The parent test sends commands; no user text is read.
import AppKit
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let menu = NSMenu()
let appItem = NSMenuItem(title: "Application", action: nil, keyEquivalent: "")
appItem.submenu = NSMenu(title: "Application"); menu.addItem(appItem)
let editItem = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
let editMenu = NSMenu(title: "Edit")
for (title, action, key) in [("Cut", "cut:", "x"), ("Copy", "copy:", "c"),
                           ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")] {
    editMenu.addItem(withTitle: title, action: Selector(action), keyEquivalent: key)
}
editItem.submenu = editMenu; menu.addItem(editItem); app.mainMenu = menu
let window = NSWindow(contentRect: NSRect(x: 280, y: 280, width: 560, height: 210),
    styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "VoiceX post-edit controlled scenarios"
let plain = NSTextField(frame: NSRect(x: 24, y: 150, width: 510, height: 28))
let search = NSSearchField(frame: NSRect(x: 24, y: 100, width: 510, height: 28))
let secure = NSSecureTextField(frame: NSRect(x: 24, y: 50, width: 510, height: 28))
secure.stringValue = "synthetic-password"
for field in [plain, search, secure] { window.contentView?.addSubview(field) }
var active: NSTextField = plain
func reply(_ text: String) { print(text); fflush(stdout) }
func command(_ line: String) {
    let parts = line.components(separatedBy: "\t")
    switch parts[0] {
    case "set":
        window.makeFirstResponder(nil)
        active = parts[1] == "plain" ? plain : search
        active.stringValue = String(data: Data(base64Encoded: parts[2])!, encoding: .utf8)!
        window.makeKeyAndOrderFront(nil); app.activate(ignoringOtherApps: true)
        window.makeFirstResponder(active)
        if let editor = active.currentEditor() as? NSTextView {
            editor.string = active.stringValue
            editor.setSelectedRange(NSRange(location: Int(parts[3])!, length: 0))
        }
    case "edit":
        let text = String(data: Data(base64Encoded: parts[1])!, encoding: .utf8)!
        active.stringValue = text
        if let editor = active.currentEditor() as? NSTextView {
            editor.string = text
            let caret = parts.count > 2 ? Int(parts[2])! : text.utf16.count
            editor.setSelectedRange(NSRange(location: caret, length: 0))
            editor.didChangeText()
        }
        NSAccessibility.post(element: active, notification: .valueChanged)
    case "secure": window.makeFirstResponder(secure)
    case "snapshot":
        let editor = active.currentEditor() as? NSTextView
        reply("synthetic snapshot: \(editor?.string ?? active.stringValue), selection=\(editor?.selectedRange() ?? NSRange(location: 0, length: 0))")
        return
    case "quit": app.terminate(nil)
    default: reply("invalid"); return
    }
    reply("ok")
}
window.makeKeyAndOrderFront(nil); app.activate(ignoringOtherApps: true)
window.makeFirstResponder(plain)
DispatchQueue.global().async {
    while let line = readLine() { DispatchQueue.main.async { command(line) } }
    DispatchQueue.main.async { app.terminate(nil) }
}
DispatchQueue.main.async { reply("ready") }
app.run()
