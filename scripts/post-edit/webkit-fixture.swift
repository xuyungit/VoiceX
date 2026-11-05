// Synthetic WebKit range tests. Run only during a foreground test slot.
import AppKit
import WebKit
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(contentRect: NSRect(x: 80, y: 80, width: 500, height: 260), styleMask: [.titled], backing: .buffered, defer: false)
window.title = "VoiceX synthetic WebKit range test"
let web = WKWebView(frame: window.contentView!.bounds)
window.contentView!.addSubview(web)
var mode = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "editable"
var commandNavigation = false
func htmlForMode() -> String {
    mode == "password" ? "<input type='password' value='synthetic-password'>" : "<body>VoiceX test A</body>"
}
func reply(_ text: String) { print(text); fflush(stdout) }
func setText(_ text: String, caret: Int, completion: @escaping () -> Void) {
    let quoted = String(data: try! JSONSerialization.data(withJSONObject: [text]), encoding: .utf8)!
    let script = """
    document.designMode='on';document.body.textContent=\(quoted)[0];
    document.body.style.whiteSpace='pre-wrap';
    const r=document.createRange();r.setStart(document.body.firstChild,\(caret));r.collapse(true);
    getSelection().removeAllRanges();getSelection().addRange(r);
    """
    web.evaluateJavaScript("(() => { \(script) })()") { _, error in
        guard error == nil else { reply("failed"); return }
        completion()
    }
}
class Ready: NSObject, WKNavigationDelegate {
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        window.makeKeyAndOrderFront(nil); app.activate(ignoringOtherApps: true)
        window.makeFirstResponder(web)
        let answer = commandNavigation ? "ok" : "ready"
        commandNavigation = false
        if mode == "editable" { setText("VoiceX test A", caret: 13) { reply(answer) } }
        else if mode == "password" {
            web.evaluateJavaScript("document.querySelector('input').focus()") { _, error in
                guard error == nil else { reply("failed"); return }
                reply(answer)
            }
        } else { reply(answer) }
    }
}
let delegate = Ready(); web.navigationDelegate = delegate
web.loadHTMLString(htmlForMode(), baseURL: nil)
window.makeKeyAndOrderFront(nil)
DispatchQueue.global().async {
    while let line = readLine() {
        DispatchQueue.main.async {
            let parts = line.components(separatedBy: "\t")
            if parts[0] == "set", parts.count == 3,
               let data = Data(base64Encoded: parts[1]), let text = String(data: data, encoding: .utf8), let caret = Int(parts[2]) {
                setText(text, caret: caret) { reply("ok") }
            } else if parts[0] == "mode", parts.count == 2, ["editable", "readonly", "password"].contains(parts[1]) {
                mode = parts[1]
                if mode == "readonly" {
                    web.evaluateJavaScript("document.designMode='off'") { _, error in
                        guard error == nil else { reply("failed"); return }
                        reply("ok")
                    }
                } else {
                    commandNavigation = true
                    web.loadHTMLString(htmlForMode(), baseURL: nil)
                }
            } else { reply("invalid") }
        }
    }
    DispatchQueue.main.async { app.terminate(nil) }
}
app.run()
