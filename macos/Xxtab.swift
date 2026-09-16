import AppKit
import Foundation
import Darwin
import UniformTypeIdentifiers

struct Draft: Codable {
    var name: String
    var tunnel: String
    var wireguard: String
}
struct Profile: Decodable { let name: String; let id: String }
struct Catalog: Decodable { let profiles: [Profile]; let selected: Int }
struct Snapshot: Decodable { let status: String; let lines: [String]; let sequence: UInt64; let finished: Bool }
struct UpdateRelease: Decodable { let version: String }
struct UpdateCheck: Decodable { let current: String; let available: Bool; let release: UpdateRelease? }
struct UpdateDownload: Decodable { let version: String; let path: String; let sha256: String }
enum AppError: LocalizedError {
    case message(String)
    var errorDescription: String? { if case let .message(value) = self { return value }; return nil }
}

// Create private files before writing, then atomically publish. In particular,
// heartbeat replacement must never expose a temporarily world-readable file.
func privateWrite(_ data: Data, to url: URL) throws {
    let temporary = url.deletingLastPathComponent().appendingPathComponent(UUID().uuidString)
    let fd = Darwin.open(temporary.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
    guard fd >= 0 else { throw AppError.message("无法创建私有配置文件") }
    let file = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
    defer { try? FileManager.default.removeItem(at: temporary) }
    try file.write(contentsOf: data)
    try file.close()
    guard Darwin.rename(temporary.path, url.path) == 0 else { throw AppError.message("无法保存配置文件") }
}
func shellQuote(_ value: String) -> String { "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'" }
func appleScriptQuote(_ value: String) -> String {
    "\"" + value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"").replacingOccurrences(of: "\n", with: "\\n").replacingOccurrences(of: "\r", with: "\\r") + "\""
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let root: URL
    let helper: URL
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 790, height: 560), styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
    let profiles = NSPopUpButton()
    let status = NSTextField(labelWithString: "未连接")
    let log = NSTextView()
    var profileButtons: [NSButton] = []
    var connectButton: NSButton!
    var disconnectButton: NSButton!
    var reconnectButton: NSButton!
    var statusItem: NSStatusItem!
    var connectionItems: [NSMenuItem] = []
    var catalog = Catalog(profiles: [], selected: 0)
    var process: Process?
    var session: URL?
    var timer: Timer?
    var stopping = false
    var restarting = false
    var closing = false
    var lastLog = ""
    var lastSequence: UInt64 = 0
    var clearAfter: UInt64 = 0
    var snapshotDate: Date?
    var editor: NSWindow?
    var editorIndex: Int?
    var original: Draft?
    var editorReadOnly = false
    var nameField: NSTextField!
    var tunnelField: NSTextView!
    var wgField: NSTextView!
    var updater: Process?
    var updateItem: NSMenuItem!
    var pendingUpdate: URL?

    init(root: URL? = nil) {
        self.root = root ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/xxtab/profiles")
        helper = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/xxtab")
        super.init()
    }
    func bridge(_ action: String, _ values: [String: Any] = [:]) throws -> Data {
        var request = values
        request["root"] = root.path; request["action"] = action
        let task = Process(); task.executableURL = helper; task.arguments = ["desktop"]
        let input = Pipe(); let output = Pipe(); let errors = Pipe()
        task.standardInput = input; task.standardOutput = output; task.standardError = errors
        try task.run()
        try input.fileHandleForWriting.write(contentsOf: JSONSerialization.data(withJSONObject: request))
        try input.fileHandleForWriting.close()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        task.waitUntilExit()
        guard task.terminationStatus == 0,
              let response = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AppError.message("无法运行配置管理程序")
        }
        guard (response["ok"] as? Bool) == true else { throw AppError.message((response["error"] as? String) ?? "配置操作失败") }
        return try JSONSerialization.data(withJSONObject: response["data"] ?? [:])
    }
    func action(_ body: () throws -> Void) {
        do { try body() } catch {
            let alert = NSAlert(); alert.messageText = "操作未完成"; alert.informativeText = error.localizedDescription
            NSApp.activate(ignoringOtherApps: true); alert.runModal()
        }
    }
    func button(_ title: String, _ selector: Selector) -> NSButton { NSButton(title: title, target: self, action: selector) }
    func row(_ views: [NSView]) -> NSStackView {
        let row = NSStackView(views: views); row.orientation = .horizontal; row.spacing = 8; return row
    }
    func scroll(_ view: NSTextView, editable: Bool) -> NSScrollView {
        view.isRichText = false; view.isEditable = editable; view.isSelectable = true
        view.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        view.isAutomaticQuoteSubstitutionEnabled = false; view.isAutomaticDashSubstitutionEnabled = false
        view.isAutomaticTextReplacementEnabled = false; view.isAutomaticSpellingCorrectionEnabled = false
        view.textContainerInset = NSSize(width: 8, height: 8)
        view.autoresizingMask = [.width]; view.isVerticallyResizable = true
        view.textContainer?.widthTracksTextView = true
        let scroll = NSScrollView(); scroll.hasVerticalScroller = true; scroll.borderType = .bezelBorder; scroll.documentView = view
        return scroll
    }
    func stack(in host: NSWindow, views: [NSView]) -> NSStackView {
        let stack = NSStackView(views: views); stack.orientation = .vertical; stack.alignment = .leading; stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = host.contentView!; content.addSubview(stack)
        NSLayoutConstraint.activate([stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 16), stack.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -16), stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 16), stack.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -16)])
        return stack
    }
    func buildUI() {
        window.title = "xxtab — WireGuard 隧道客户端"; window.minSize = NSSize(width: 640, height: 440)
        window.delegate = self; window.isReleasedWhenClosed = false; window.center()
        profiles.target = self; profiles.action = #selector(selectProfile)
        connectButton = button("连接", #selector(connect)); disconnectButton = button("断开", #selector(disconnect)); reconnectButton = button("重新连接", #selector(reconnect))
        profileButtons = [button("新建配置", #selector(newProfile)), button("导入配置…", #selector(importProfile)), button("编辑配置…", #selector(editProfile)), button("查看当前配置", #selector(viewProfile))]
        let logs = scroll(log, editable: false)
        let controls = row([connectButton, disconnectButton, reconnectButton])
        let title = row([NSTextField(labelWithString: "运行日志"), button("清空日志", #selector(clearLog))])
        let layout = stack(in: window, views: [profiles, row([status, controls]), row(profileButtons), title, logs])
        profiles.widthAnchor.constraint(equalTo: layout.widthAnchor).isActive = true
        logs.widthAnchor.constraint(equalTo: layout.widthAnchor).isActive = true
        logs.heightAnchor.constraint(greaterThanOrEqualToConstant: 180).isActive = true
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.title = "X"; statusItem.button?.font = .boldSystemFont(ofSize: 15)
        let menu = NSMenu(); menu.autoenablesItems = false
        let show = NSMenuItem(title: "显示窗口", action: #selector(showMain), keyEquivalent: ""); show.target = self; menu.addItem(show)
        menu.addItem(.separator())
        for (title, selector) in [("连接", #selector(connect)), ("断开", #selector(disconnect)), ("重新连接", #selector(reconnect))] {
            let item = NSMenuItem(title: title, action: selector, keyEquivalent: ""); item.target = self; menu.addItem(item); connectionItems.append(item)
        }
        menu.addItem(.separator()); let quit = NSMenuItem(title: "退出", action: #selector(quitApp), keyEquivalent: "q"); quit.target = self; menu.addItem(quit)
        statusItem.menu = menu
        let main = NSMenu(); let app = NSMenuItem(); main.addItem(app); let appMenu = NSMenu(); app.submenu = appMenu
        updateItem = NSMenuItem(title: "检查更新…", action: #selector(checkUpdate), keyEquivalent: "")
        updateItem.target = self; appMenu.autoenablesItems = false; appMenu.addItem(updateItem); appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "退出 xxtab", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        let edit = NSMenuItem(); main.addItem(edit); let editMenu = NSMenu(title: "编辑"); edit.submenu = editMenu
        for (title, selector, key) in [("撤销", "undo:", "z"), ("剪切", "cut:", "x"), ("复制", "copy:", "c"), ("粘贴", "paste:", "v"), ("全选", "selectAll:", "a")] {
            editMenu.addItem(withTitle: title, action: Selector(selector), keyEquivalent: key)
        }
        NSApp.mainMenu = main
    }
    func applicationDidFinishLaunching(_ notification: Notification) {
        buildUI(); action { try reloadProfiles() }; updateControls(); showMain()
        timer = Timer(timeInterval: 1, target: self, selector: #selector(poll), userInfo: nil, repeats: true)
        RunLoop.main.add(timer!, forMode: .common)
        RunLoop.main.add(timer!, forMode: .modalPanel)
    }
    func reloadProfiles() throws {
        catalog = try JSONDecoder().decode(Catalog.self, from: bridge("list"))
        profiles.removeAllItems(); profiles.addItems(withTitles: catalog.profiles.map { $0.name })
        if !catalog.profiles.isEmpty { profiles.selectItem(at: min(catalog.selected, catalog.profiles.count - 1)) }
        updateControls()
    }
    func selectedDraft() throws -> Draft {
        guard profiles.indexOfSelectedItem >= 0 else { throw AppError.message("请先导入或新建配置") }
        return try JSONDecoder().decode(Draft.self, from: bridge("load", ["index": profiles.indexOfSelectedItem]))
    }
    func updateControls() {
        updateItem?.isEnabled = updater == nil && !closing
        let busy = process != nil; let selected = profiles.indexOfSelectedItem >= 0; let modal = editor != nil
        profiles.isEnabled = !busy && !modal
        let states = [!busy && selected && !closing && !modal, busy && !stopping && !modal, busy && !stopping && !modal]
        for (button, enabled) in zip([connectButton, disconnectButton, reconnectButton], states) { button?.isEnabled = enabled }
        for (item, enabled) in zip(connectionItems, states) { item.isEnabled = enabled }
        for (index, button) in profileButtons.enumerated() { button.isEnabled = !modal && (index == 3 ? selected : (!busy && (index < 2 || selected))) }
        statusItem?.button?.toolTip = "xxtab · " + status.stringValue
    }
    @objc func selectProfile() { action { _ = try bridge("select", ["index": profiles.indexOfSelectedItem]) }; updateControls() }
    @objc func showMain() { NSApp.activate(ignoringOtherApps: true); window.deminiaturize(nil); window.makeKeyAndOrderFront(nil); editor?.makeKeyAndOrderFront(nil) }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool { showMain(); return true }
    func windowDidMiniaturize(_ notification: Notification) { if (notification.object as? NSWindow) === window { window.orderOut(nil) } }
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if sender === window { window.orderOut(nil); return false }
        if !editorReadOnly, let original = original {
            let now = draftFromEditor()
            if now.name != original.name || now.tunnel != original.tunnel || now.wireguard != original.wireguard {
                let alert = NSAlert(); alert.messageText = "放弃未保存的修改？"; alert.addButton(withTitle: "继续编辑"); alert.addButton(withTitle: "放弃")
                if alert.runModal() != .alertSecondButtonReturn { return false }
            }
        }
        return true
    }
    func windowWillClose(_ notification: Notification) {
        if (notification.object as? NSWindow) === editor {
            editor = nil; original = nil; nameField = nil; tunnelField = nil; wgField = nil; updateControls()
        }
    }
    func filePanel(wgOnly: Bool = false) -> URL? {
        let panel = NSOpenPanel(); panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        panel.allowedContentTypes = (wgOnly ? ["conf"] : ["toml", "conf"]).compactMap { UTType(filenameExtension: $0) }
        return panel.runModal() == .OK ? panel.url : nil
    }
    func openEditor(_ draft: Draft, index: Int?, readOnly: Bool) {
        if editor != nil { editor?.makeKeyAndOrderFront(nil); return }
        let host = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 860, height: 700), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        editor = host; editorIndex = index; original = draft; editorReadOnly = readOnly
        host.title = readOnly ? "查看当前配置" : "编辑配置"; host.delegate = self; host.isReleasedWhenClosed = false; host.minSize = NSSize(width: 660, height: 520); host.center()
        nameField = NSTextField(string: draft.name); nameField.isEditable = !readOnly
        tunnelField = NSTextView(); tunnelField.string = draft.tunnel
        wgField = NSTextView(); wgField.string = draft.wireguard
        let tunnel = scroll(tunnelField, editable: !readOnly); let wg = scroll(wgField, editable: !readOnly)
        var buttons = [button("关闭", #selector(closeEditor))]
        if !readOnly { buttons = [button("导入 WireGuard…", #selector(importWG)), button("保存", #selector(saveEditor))] + buttons }
        let layout = stack(in: host, views: [nameField, NSTextField(labelWithString: "隧道配置 · xxtab.toml"), tunnel, NSTextField(labelWithString: "WireGuard 配置"), wg, row(buttons)])
        for view in [nameField! as NSView, tunnel, wg] { view.widthAnchor.constraint(equalTo: layout.widthAnchor).isActive = true }
        tunnel.heightAnchor.constraint(equalTo: wg.heightAnchor).isActive = true
        tunnel.heightAnchor.constraint(greaterThanOrEqualToConstant: 150).isActive = true
        host.makeKeyAndOrderFront(nil); updateControls()
    }
    func draftFromEditor() -> Draft { Draft(name: nameField.stringValue, tunnel: tunnelField.string, wireguard: wgField.string) }
    @objc func newProfile() { action { openEditor(try JSONDecoder().decode(Draft.self, from: bridge("template")), index: nil, readOnly: false) } }
    @objc func importProfile() { guard let url = filePanel() else { return }; action { openEditor(try JSONDecoder().decode(Draft.self, from: bridge("import", ["path": url.path])), index: nil, readOnly: false) } }
    @objc func editProfile() { action { openEditor(try selectedDraft(), index: profiles.indexOfSelectedItem, readOnly: false) } }
    @objc func viewProfile() { action { openEditor(try selectedDraft(), index: profiles.indexOfSelectedItem, readOnly: true) } }
    @objc func importWG() {
        guard !editorReadOnly, let url = filePanel(wgOnly: true) else { return }
        action { let draft = try JSONDecoder().decode(Draft.self, from: bridge("import", ["path": url.path])); wgField.string = draft.wireguard }
    }
    @objc func closeEditor() { editor?.performClose(nil) }
    @objc func saveEditor() {
        guard !editorReadOnly else { return }
        action {
            let draft = draftFromEditor(); let value = try JSONSerialization.jsonObject(with: JSONEncoder().encode(draft))
            _ = try bridge("save", ["index": editorIndex.map { $0 as Any } ?? NSNull(), "draft": value])
            original = draft; editor?.close(); try reloadProfiles()
        }
    }
    @objc func clearLog() { clearAfter = lastSequence; log.string = ""; lastLog = "" }
    @objc func checkUpdate() { startUpdate() }
    func startUpdate(version: String? = nil) {
        guard updater == nil && !closing else { return }
        action {
            let task = Process(); task.executableURL = helper
            task.arguments = version.map { ["update", "download", $0] } ?? ["update", "check"]
            let output = Pipe(); let errors = Pipe()
            task.standardOutput = output; task.standardError = errors; task.standardInput = FileHandle.nullDevice
            task.terminationHandler = { [weak self] task in
                let data = output.fileHandleForReading.readDataToEndOfFile()
                let error = String(data: errors.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? "更新失败"
                DispatchQueue.main.async { self?.updateEnded(task.terminationStatus, data: data, error: error, downloading: version != nil) }
            }
            updater = task; updateItem.title = version == nil ? "正在检查更新…" : "正在下载并校验…"; updateControls()
            do { try task.run() }
            catch { updater = nil; updateItem.title = "检查更新…"; updateControls(); throw error }
        }
    }
    func updateEnded(_ code: Int32, data: Data, error: String, downloading: Bool) {
        updater = nil; updateItem.title = "检查更新…"; updateControls()
        guard !closing else { return }
        action {
            guard code == 0 else { throw AppError.message(String(error.prefix(2048))) }
            let alert = NSAlert(); NSApp.activate(ignoringOtherApps: true)
            if downloading {
                let download = try JSONDecoder().decode(UpdateDownload.self, from: data)
                alert.messageText = "更新 \(download.version) 已下载并通过 SHA256 校验"
                alert.informativeText = "是否断开连接并退出，打开更新安装包？\n打开后将 xxtab 拖到 Applications 替换旧版本。已有配置会保留。\n\n安装包：\(download.path)"
                alert.addButton(withTitle: "退出并打开安装包"); alert.addButton(withTitle: "稍后安装")
                if alert.runModal() == .alertFirstButtonReturn {
                    pendingUpdate = URL(fileURLWithPath: download.path); NSApp.terminate(nil)
                }
            } else {
                let result = try JSONDecoder().decode(UpdateCheck.self, from: data)
                if result.available, let release = result.release {
                    alert.messageText = "发现新版本 \(release.version)"
                    alert.informativeText = "当前版本：\(result.current)\n从 GitHub Releases 下载此 Mac 架构的安装包？"
                    alert.addButton(withTitle: "下载更新"); alert.addButton(withTitle: "取消")
                    if alert.runModal() == .alertFirstButtonReturn { startUpdate(version: release.version) }
                } else {
                    alert.messageText = result.release == nil ? "GitHub 尚未发布正式版本" : "没有更新的正式版本"
                    alert.informativeText = "当前版本：\(result.current)"; alert.runModal()
                }
            }
        }
    }
    @objc func connect() {
        guard process == nil && !closing && editor == nil else { return }
        action {
            let dependencies = try JSONSerialization.jsonObject(with: bridge("dependencies")) as? [String: Any]
            guard (dependencies?["ready"] as? Bool) == true else { throw AppError.message("请先在终端安装系统 WireGuard 依赖：\nbrew install bash wireguard-tools wireguard-go") }
            let draft = try selectedDraft(); _ = try bridge("select", ["index": profiles.indexOfSelectedItem])
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("xxtab-session-" + UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            do {
                try privateWrite(JSONEncoder().encode(draft), to: directory.appendingPathComponent("request.json"))
                try privateWrite(Data(UUID().uuidString.utf8), to: directory.appendingPathComponent("heartbeat"))
                let command = "exec " + shellQuote(helper.path) + " macos-session " + shellQuote(directory.path) + " " + String(getuid())
                let script = "do shell script " + appleScriptQuote(command) + " with administrator privileges"
                let task = Process(); task.executableURL = URL(fileURLWithPath: "/usr/bin/osascript"); task.arguments = ["-e", script]
                let errors = Pipe(); task.standardError = errors; task.standardOutput = FileHandle.nullDevice; task.standardInput = FileHandle.nullDevice
                task.terminationHandler = { [weak self] task in
                    let error = String(data: errors.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
                    DispatchQueue.main.async { self?.ended(task.terminationStatus, error: error) }
                }
                session = directory; process = task; stopping = false; restarting = false; lastLog = ""; log.string = ""
                lastSequence = 0; clearAfter = 0; snapshotDate = nil
                status.stringValue = "等待管理员授权／正在连接…"; updateControls()
                try task.run()
            } catch { process = nil; session = nil; try? FileManager.default.removeItem(at: directory); updateControls(); throw error }
        }
    }
    @objc func poll() {
        guard let directory = session else { return }
        do { try privateWrite(Data(UUID().uuidString.utf8), to: directory.appendingPathComponent("heartbeat")) }
        catch { status.stringValue = "会话心跳失败，将自动断开" }
        readSnapshot(directory); updateControls()
    }
    func readSnapshot(_ directory: URL) {
        // A snapshot can be read during a bounded in-place rewrite; retry next tick.
        let path = directory.appendingPathComponent("snapshot.json")
        guard let metadata = try? FileManager.default.attributesOfItem(atPath: path.path),
              let date = metadata[.modificationDate] as? Date, date != snapshotDate,
              let size = metadata[.size] as? NSNumber, size.intValue <= 1024 * 1024,
              let data = try? Data(contentsOf: path),
              let snapshot = try? JSONDecoder().decode(Snapshot.self, from: data) else { return }
        snapshotDate = date; lastSequence = snapshot.sequence
        let count = Int(min(UInt64(snapshot.lines.count), snapshot.sequence > clearAfter ? snapshot.sequence - clearAfter : 0))
        let text = snapshot.lines.suffix(count).joined(separator: "\n")
        if text != lastLog { lastLog = text; log.string = text; log.scrollToEndOfDocument(nil) }
        let labels = ["Idle": "未连接", "Connecting": "正在连接…", "Connected": "隧道已连接", "Reconnecting": "正在重新连接…", "Failed": "连接失败，请查看日志"]
        status.stringValue = stopping ? "正在断开并清理…" : (labels[snapshot.status] ?? snapshot.status)
    }
    func stop(restart: Bool) {
        guard let directory = session else { return }
        restarting = restart; stopping = true
        action { try privateWrite(Data("stop".utf8), to: directory.appendingPathComponent("stop")) }
        status.stringValue = "正在断开并清理…"; updateControls()
    }
    @objc func disconnect() { if !stopping { stop(restart: false) } }
    @objc func reconnect() { if !stopping { stop(restart: true) } }
    func ended(_ code: Int32, error: String) {
        if let directory = session { readSnapshot(directory); try? FileManager.default.removeItem(at: directory) }
        session = nil; process = nil; stopping = false
        if code != 0 { status.stringValue = "连接未完成或授权已取消"; log.string += "\n" + String(error.prefix(2048)) }
        else { status.stringValue = "未连接" }
        updateControls()
        if closing {
            if pendingUpdate != nil && code != 0 {
                pendingUpdate = nil; closing = false; updateControls()
                NSApp.reply(toApplicationShouldTerminate: false)
                action { throw AppError.message("连接清理未成功，已取消更新安装，请查看日志。") }
            } else { NSApp.reply(toApplicationShouldTerminate: true) }
            return
        }
        let retry = restarting && code == 0; restarting = false
        if retry { connect() }
    }
    @objc func quitApp() { NSApp.terminate(nil) }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if let editor = editor, !windowShouldClose(editor) { pendingUpdate = nil; return .terminateCancel }
        if process != nil { closing = true; stop(restart: false); return .terminateLater }
        return .terminateNow
    }
    func applicationWillTerminate(_ notification: Notification) {
        timer?.invalidate(); updater?.terminate()
        if let item = statusItem { NSStatusBar.system.removeStatusItem(item) }
        if let package = pendingUpdate { NSWorkspace.shared.open(package) }
    }
}

@main
struct XxtabApp {
    @MainActor
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let smoke = CommandLine.arguments.contains("--smoke-test")
        if !smoke, let identifier = Bundle.main.bundleIdentifier {
            let other = NSRunningApplication.runningApplications(withBundleIdentifier: identifier).first { $0.processIdentifier != getpid() }
            if let other = other { other.activate(options: [.activateAllWindows, .activateIgnoringOtherApps]); exit(0) }
        }
        let smokeRoot = FileManager.default.temporaryDirectory.appendingPathComponent("xxtab-smoke-" + UUID().uuidString)
        let delegate = AppDelegate(root: smoke ? smokeRoot : nil)
        app.delegate = delegate
        if smoke {
            do {
                defer { try? FileManager.default.removeItem(at: smokeRoot) }
                delegate.buildUI(); try delegate.reloadProfiles()
                let draft = try JSONDecoder().decode(Draft.self, from: delegate.bridge("template"))
                precondition(draft.tunnel.contains("127.0.0.1:51820")); precondition(draft.wireguard.contains("Endpoint = 127.0.0.1:51820"))
                delegate.openEditor(draft, index: nil, readOnly: true)
                precondition(delegate.wgField.isEditable == false)
                delegate.editor?.close()
                print("PASS: native AppKit controls, profile bridge, templates, read-only viewer")
                return
            } catch { fputs("macOS smoke test failed: \(error.localizedDescription)\n", stderr); exit(1) }
        }
        // NSApplication's delegate is weak; retain it for the entire event loop.
        withExtendedLifetime(delegate) { app.run() }
    }
}
