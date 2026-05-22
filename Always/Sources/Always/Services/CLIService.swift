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
        let status = try await getStatus()
        if status.isRunning {
            Self.logger.info("Daemon already running — skipping start")
            return "Always-on daemon already running."
        }
        if !AppDelegate.listDaemonProcessIDs().isEmpty {
            Self.logger.warning("Daemon process without pid file — skipping start")
            return "Always-on daemon already running."
        }
        // Read current config to pass to daemon. Only pass flags the user
        // has explicitly opted into; the daemon loads stt_silence,
        // auto_enter_delay_ms, energy thresholds, etc. from its own prefs
        // table so we don't have to mirror every knob through CLI args.
        let config = try await getConfig()
        var args = ["start"]
        if config.sttAutoEnter {
            args.append("--auto-enter")
        }
        return try await runCLI(arguments: args)
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
        let homebrewBin = "/opt/homebrew/bin"
        let existingPath = env["PATH"] ?? ""
        if !existingPath.split(separator: ":").contains(Substring(homebrewBin)) {
            env["PATH"] = existingPath.isEmpty
                ? homebrewBin
                : "\(homebrewBin):\(existingPath)"
        }
        // Pass through GROQ_API_KEY from parent environment if set.
        if let groqKey = ProcessInfo.processInfo.environment["GROQ_API_KEY"] {
            env["GROQ_API_KEY"] = groqKey
        }
        process.environment = env

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        try process.run()
        process.waitUntilExit()

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let output = String(data: data, encoding: .utf8) ?? ""

        if process.terminationStatus != 0 {
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