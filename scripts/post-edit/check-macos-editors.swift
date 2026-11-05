import AppKit
import Foundation

// libtest does not run AppKit's main loop. Detect foreground changes here,
// with a live main run loop, then start a fresh test process for each target.
// The launcher reads process identity only, never AX controls or draft text.
let defaults = ["com.anysphere.sand", "com.tencent.workbuddy.mac", "com.deepseek.dsh"]
let allowed = Set(defaults + ["md.obsidian"])
guard CommandLine.arguments.count >= 2 else {
    fputs("Usage: swift scripts/post-edit/check-macos-editors.swift /absolute/path/to/prebuilt-test-binary [bundle-id ...]\n", stderr)
    exit(2)
}
let binary = CommandLine.arguments[1]
let targets = CommandLine.arguments.count > 2 ? Array(CommandLine.arguments.dropFirst(2)) : defaults
guard targets.allSatisfy({ allowed.contains($0) }) else {
    fputs("Unexpected test target\n", stderr)
    exit(2)
}
guard FileManager.default.isExecutableFile(atPath: binary) else {
    fputs("Prebuilt test binary is not executable\n", stderr)
    exit(2)
}
func emit(_ value: String) { print(value); fflush(stdout) }
func runChecks() throws -> Int32 {
    let directory = FileManager.default.temporaryDirectory
        .appendingPathComponent("voicex-editor-tests-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: directory) }
    var failed = false
    emit("Ready: \(targets.joined(separator: " → ")). In each empty editor, type VoiceX test A, wait 3 s, change A to B, wait 3 s. Do not send. For Obsidian, use an empty temporary note in Source mode.")
    for bundle in targets {
        let deadline = Date(timeIntervalSinceNow: 90)
        emit("waiting for foreground: \(bundle)")
        while NSWorkspace.shared.frontmostApplication?.bundleIdentifier != bundle {
            guard Date() < deadline else {
                fputs("Timed out waiting for foreground: \(bundle)\n", stderr)
                return 1
            }
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        }
        let pid = NSWorkspace.shared.frontmostApplication!.processIdentifier
        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.arguments = ["post_edit::platform::macos_tests::live_macos_electron_editors", "--exact", "--ignored", "--nocapture", "--test-threads=1"]
        var environment = ProcessInfo.processInfo.environment
        environment["VOICEX_POST_EDIT_EDITOR_PID"] = String(pid)
        process.environment = environment
        let log = directory.appendingPathComponent("\(bundle).log")
        FileManager.default.createFile(atPath: log.path, contents: nil)
        let output = try FileHandle(forWritingTo: log)
        process.standardOutput = output
        process.standardError = output
        process.standardInput = FileHandle.nullDevice
        try process.run()
        emit("started native test: \(bundle), pid=\(pid)")
        while process.isRunning {
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        }
        try output.close()
        emit("finished native test: \(bundle), status=\(process.terminationStatus)")
        emit(try String(contentsOf: log, encoding: .utf8))
        failed = failed || process.terminationStatus != 0
    }
    return failed ? 1 : 0
}
exit(try runChecks())
