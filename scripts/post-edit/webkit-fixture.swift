// Synthetic WebKit range tests. The window stays behind the user's apps.
import AppKit
import WebKit
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let window = NSWindow(contentRect: NSRect(x: 80, y: 80, width: 500, height: 260), styleMask: [.titled], backing: .buffered, defer: false)
window.title = "VoiceX synthetic WebKit range test"
let web = WKWebView(frame: window.contentView!.bounds)
window.contentView!.addSubview(web)
let mode = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "editable"
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
        app.unhideWithoutActivation(); window.orderBack(nil)
        if mode == "editable" { setText("VoiceX test A", caret: 13) { reply("ready") } }
        else { reply("ready") }
    }
}
let delegate = Ready(); web.navigationDelegate = delegate
let html = mode == "password" ? "<input type='password' value='synthetic-password'>" : "<body>VoiceX test A</body>"
web.loadHTMLString(html, baseURL: nil)
window.orderBack(nil)
DispatchQueue.global().async {
    while let line = readLine() {
        DispatchQueue.main.async {
            let parts = line.components(separatedBy: "\t")
            if parts[0] == "set", parts.count == 3,
               let data = Data(base64Encoded: parts[1]), let text = String(data: data, encoding: .utf8), let caret = Int(parts[2]) {
                setText(text, caret: caret) { reply("ok") }
            } else { reply("invalid") }
        }
    }
    DispatchQueue.main.async { app.terminate(nil) }
}
app.run()
