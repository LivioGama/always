import Foundation
import os.log

class CLIService: ObservableObject {
    private let cliPath: String
    private static let logger = Logger(subsystem: "com.always.app", category: "cli-service")
    init() {
        // Path to the bundled daemon binary. Named `always-daemon` (not
        // `always`) because macOS APFS is case-insensitive by default — a
        // file named `MacOS/always` would collide with the GUI binary
        // `MacOS/Always` and silently overwrite it. See `Always/build.sh`
        // for the matching `cp` target.
        let bundle = Bundle.main
        let bundlePath = bundle.bundlePath
        let daemonPath = bundlePath + "/Contents/MacOS/always-daemon"
        if FileManager.default.fileExists(atPath: daemonPath) {
            cliPath = daemonPath
            Self.logger.info("Using bundled daemon at \(daemonPath, privacy: .public)")
            return
        }

        // Fallback for development: look in target/release. Logged as a
        // warning because in a shipped /Applications bundle this branch
        // means `build.sh` did not bundle the daemon — almost certainly a
        // packaging bug worth flagging.
        let currentPath = URL(fileURLWithPath: #file)
        let projectPath = currentPath.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        cliPath = projectPath.appendingPathComponent("target/release/always").path
        Self.logger.warning("Bundled daemon missing at \(daemonPath, privacy: .public) — falling back to dev path \(self.cliPath, privacy: .public)")
    }

    func startDaemon() async throws -> String {
        if AppDelegate.isHealthyDaemon() || AppDelegate.isDaemonSocketLive() {
            Self.logger.info("Live UDS socket — skipping start")
            return "Always-on daemon already running."
        }

        if AppDelegate.listDaemonProcessIDs().isEmpty {
            try spawnDetachedRunProcess()
        } else {
            Self.logger.warning("Daemon process without live socket — waiting for UDS bind")
        }

        let deadline = Date().addingTimeInterval(15)
        while Date() < deadline {
            if AppDelegate.isDaemonSocketLive() {
                return "Always-on daemon ready."
            }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        throw CLIError.commandFailed("Always-on daemon failed to start")
    }

    /// Spawn `always run` directly — skips the parent `start` subprocess that
    /// reloads config and can add seconds before the child even launches.
    private func spawnDetachedRunProcess() throws {
        if !FileManager.default.fileExists(atPath: cliPath) {
            throw CLIError.binaryNotFound
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: cliPath)
        // Omit `--silence` so `AlwaysConfig::from_cli` honors the saved DB
        // preference. Passing a hardcoded value here made the GUI-launched
        // daemon ignore user latency tuning on every restart.
        process.arguments = ["run", "--lang", "auto", "--timeout", "30"]
        var env = ProcessInfo.processInfo.environment
        applyCLIEnvironment(&env)
        process.environment = env
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        Self.logger.info("Spawned detached always run (pid \(process.processIdentifier))")
    }

    private func applyCLIEnvironment(_ env: inout [String: String]) {
        let homebrewBin = "/opt/homebrew/bin"
        let existingPath = env["PATH"] ?? ""
        if !existingPath.split(separator: ":").contains(Substring(homebrewBin)) {
            env["PATH"] = existingPath.isEmpty
                ? homebrewBin
                : "\(homebrewBin):\(existingPath)"
        }
        env.removeValue(forKey: "GROQ_API_KEY")
        env["ALWAYS_SKIP_ORPHAN_KILL"] = "1"
        // Marks the daemon as GUI-owned: it mutes dictation shortly after
        // the last UDS client disconnects and exits soon after, instead of
        // pasting transcripts while the app is closed. A terminal-launched
        // `always run` doesn't have this and keeps running standalone.
        env["ALWAYS_SPAWNED_BY_GUI"] = "1"
    }

    func stopDaemon() async throws -> String {
        try await runCLI(arguments: ["stop"])
    }

    /// Force-kill any existing daemon and start fresh. Handles stale daemons
    /// that survived a crash or force-quit.
    func restartDaemon() async throws -> String {
        AppDelegate.killStaleDaemon()
        try await Task.sleep(nanoseconds: 300_000_000)
        return try await startDaemon()
    }

    func getStatus() async throws -> DaemonStatus {
        let output = try await runCLI(arguments: ["status"])
        return DaemonStatus.fromCLI(output: output) ?? DaemonStatus(isRunning: false, pid: nil, logPath: nil)
    }

    func getConfig() async throws -> Config {
        let output = try await runCLI(arguments: ["config", "show"])
        return Config.fromCLI(output: output) ?? Config.defaultConfig
    }

    func setConfig(key: String, value: String) async throws -> String {
        try await runCLI(arguments: ["config", "set", key, value])
    }

    func togglePause() async throws -> String {
        try await runCLI(arguments: ["toggle-pause"])
    }

    func toggleAutoEnter() async throws -> String {
        try await runCLI(arguments: ["toggle-auto-enter"])
    }

    func getState() async throws -> DaemonState {
        let output = try await runCLI(arguments: ["get-state"])
        return DaemonState.fromCLI(output: output) ?? DaemonState(isPaused: false, isAutoEnter: false)
    }

    private func runCLI(arguments: [String]) async throws -> String {
        // Check if binary exists
        if !FileManager.default.fileExists(atPath: cliPath) {
            throw CLIError.binaryNotFound
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: cliPath)
        process.arguments = arguments

        // Set environment variables. The daemon needs `/opt/homebrew/bin`
        // on PATH so the bundled audio pipeline can find SoX `rec`. Use
        // `??` to handle the case where PATH is genuinely unset (no
        // shell environment inheritance) — otherwise the
        // `env["PATH"] = "\(homebrewBin):\(env["PATH"] ?? "")"` line
        // would dereference a nil and the subprocess inherits an empty
        // PATH, breaking SoX lookup silently.
        var env = ProcessInfo.processInfo.environment
        applyCLIEnvironment(&env)
        process.environment = env

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        // Run the blocking process work off the Swift concurrency cooperative
        // pool (Task.detached) so a slow/hung daemon subcommand cannot starve
        // other async work. Inside, drain the pipe to EOF *before* waiting:
        // both stdout and stderr feed the same Pipe, so if the child emits
        // more than the OS pipe buffer (~16-64 KB) it blocks on write and
        // never exits. `readDataToEndOfFile()` returns when the child closes
        // the write end (EOF on exit), avoiding the wait-then-drain deadlock.
        let (data, status) = try await Task.detached(priority: .utility) {
            try process.run()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return (data, process.terminationStatus)
        }.value

        let output = String(data: data, encoding: .utf8) ?? ""

        if status != 0 {
            throw CLIError.commandFailed(output)
        }

        return output
    }
}

enum CLIError: LocalizedError {
    case commandFailed(String)
    case binaryNotFound

    var errorDescription: String? {
        switch self {
        case .commandFailed(let message):
            return "CLI command failed: \(message)"
        case .binaryNotFound:
            return "Always binary not found"
        }
    }
}
