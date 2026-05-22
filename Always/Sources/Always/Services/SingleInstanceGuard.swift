import AppKit
import Foundation
import os.log

/// Ensures exactly one Always GUI process system-wide, regardless of bundle
/// ID (com.always, com.always.v2, stale Desktop copies, dev builds).
///
/// Uses an advisory flock on `always.gui.lock` plus a process sweep so two
/// copies cannot both own the menu bar or spawn duplicate daemons.
final class SingleInstanceGuard {
    static let shared = SingleInstanceGuard()

    private static let logger = Logger(subsystem: "com.always.app", category: "single-instance")

    /// All bundle IDs ever shipped — a second copy with a different ID still
    /// counts as a duplicate if it runs the Always GUI binary.
    static let knownBundleIDs = ["com.always.v2", "com.always", "com.alwaysapp"]

    private var lockFD: Int32 = -1
    private let lockPath: String

    private init() {
        let dir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/always", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        lockPath = dir.appendingPathComponent("always.gui.lock").path
    }

    deinit {
        release()
    }

    /// Acquire the GUI lock or hand off to the running instance. Returns
    /// `false` when this process should exit immediately.
    func acquireOrHandOff() -> Bool {
        if tryAcquireLock() {
            terminateOtherGUIProcesses(except: ProcessInfo.processInfo.processIdentifier)
            Self.logger.info("Single-instance lock acquired")
            return true
        }
        Self.logger.notice("Another Always instance holds the lock — handing off")
        activateExistingInstance()
        return false
    }

    func release() {
        guard lockFD >= 0 else { return }
        flock(lockFD, LOCK_UN)
        close(lockFD)
        lockFD = -1
    }

    // MARK: - Process discovery

    /// PIDs of Always GUI processes (never `always-daemon`).
    static func listGUIProcessIDs(except: pid_t? = nil) -> [pid_t] {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/ps")
        proc.arguments = ["-ax", "-o", "pid=,command="]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        try? proc.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()

        let text = String(data: data, encoding: .utf8) ?? ""
        var pids: [pid_t] = []
        for line in text.split(whereSeparator: \.isNewline) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard let space = trimmed.firstIndex(where: { $0.isWhitespace }) else { continue }
            let pidStr = trimmed[..<space]
            let command = String(trimmed[space...].trimmingCharacters(in: .whitespaces))
            guard let pid = pid_t(pidStr) else { continue }
            if let except, pid == except { continue }
            if isAlwaysGUIProcess(command: command) {
                pids.append(pid)
            }
        }
        return pids
    }

    static func isAlwaysGUIProcess(command: String) -> Bool {
        if command.contains("always-daemon") { return false }
        if command.contains("Always.app/Contents/MacOS/Always") { return true }
        // Local `swift run` / `.build/.../Always` dev launches.
        if command.hasSuffix("/Always") && !command.contains("Helper") { return true }
        return false
    }

    // MARK: - Private

    private func tryAcquireLock() -> Bool {
        lockFD = open(lockPath, O_CREAT | O_RDWR, 0o600)
        guard lockFD >= 0 else {
            Self.logger.error("Could not open GUI lock file — allowing launch")
            return true
        }
        return flock(lockFD, LOCK_EX | LOCK_NB) == 0
    }

    private func activateExistingInstance() {
        let myPid = ProcessInfo.processInfo.processIdentifier
        for bundleId in Self.knownBundleIDs {
            for app in NSRunningApplication.runningApplications(withBundleIdentifier: bundleId)
                where app.processIdentifier != myPid {
                app.activate(options: [.activateAllWindows, .activateIgnoringOtherApps])
                return
            }
        }
        if let pid = Self.listGUIProcessIDs(except: myPid).first,
           let app = NSRunningApplication(processIdentifier: pid) {
            app.activate(options: [.activateAllWindows, .activateIgnoringOtherApps])
        }
    }

    private func terminateOtherGUIProcesses(except: pid_t) {
        for pid in Self.listGUIProcessIDs(except: except) {
            Self.logger.warning("Terminating duplicate Always GUI pid \(pid)")
            kill(pid, SIGTERM)
            for _ in 0..<20 {
                if kill(pid, 0) != 0 { break }
                usleep(50_000)
            }
            if kill(pid, 0) == 0 {
                kill(pid, SIGKILL)
            }
        }
    }
}
