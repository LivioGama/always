import Foundation

class CLIService: ObservableObject {
    private let cliPath: String

    init() {
        // Path to the always binary - embedded in app bundle
        let bundle = Bundle.main
        let bundlePath = bundle.bundlePath
        let daemonPath = bundlePath + "/Contents/MacOS/always"
        if FileManager.default.fileExists(atPath: daemonPath) {
            cliPath = daemonPath
            print("CLIService: Using bundled daemon at \(daemonPath)")
            return
        }
        
        // Fallback for development: look in target/release
        let currentPath = URL(fileURLWithPath: #file)
        let projectPath = currentPath.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        cliPath = projectPath.appendingPathComponent("target/release/always").path
        print("CLIService: Using development daemon at \(cliPath)")
    }

    func startDaemon() async throws -> String {
        // Read current config to pass to daemon
        let config = try await getConfig()
        var args = ["start"]
        args.append("--silence")
        args.append("1.5") // Default silence threshold
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
        // Brief pause so the OS reclaims the socket and PID file
        try await Task.sleep(nanoseconds: 300_000_000) // 300ms
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

        // Set environment variables
        var env = ProcessInfo.processInfo.environment
        // Add Homebrew to PATH for SoX
        let homebrewBin = "/opt/homebrew/bin"
        if env["PATH"]?.contains(homebrewBin) == false {
            env["PATH"] = "\(homebrewBin):\(env["PATH"] ?? "")"
        }
        // Pass through GROQ_API_KEY from parent environment if set
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