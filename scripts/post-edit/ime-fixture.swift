// Synthetic native marked-text states. Run only during a foreground test slot.
import AppKit

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(contentRect: NSRect(x: 80, y: 80, width: 480, height: 180),
    styleMask: [.titled], backing: .buffered, defer: false)
window.title = "VoiceX synthetic marked-text test"
let editor = NSTextView(frame: window.contentView!.bounds)
editor.isRichText = false
editor.string = "支座返力计算"
window.contentView!.addSubview(editor)
window.makeFirstResponder(editor)
func reply(_ text: String) { print(text); fflush(stdout) }
DispatchQueue.global().async {
    while let line = readLine() {
        DispatchQueue.main.async {
            switch line {
            case "mark":
                editor.setMarkedText("fanli", selectedRange: NSRange(location: 5, length: 0),
                    replacementRange: NSRange(location: 2, length: 2))
            case "commit":
                editor.unmarkText()
                editor.string = "支座反力计算"
            default: reply("invalid"); return
            }
            reply("ok")
        }
    }
    DispatchQueue.main.async { app.terminate(nil) }
}
let readyObserver = NotificationCenter.default.addObserver(forName: NSApplication.didFinishLaunchingNotification, object: app, queue: .main) { _ in
    window.makeKeyAndOrderFront(nil)
    app.activate(ignoringOtherApps: true)
    reply("ready")
}
app.run()
