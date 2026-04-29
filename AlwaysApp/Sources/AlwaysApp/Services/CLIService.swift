import Foundation

class CLIService: ObservableObject {
    private let cliPath: String

    init() {
        // Path to the always binary - assume it's in ../target/release-fast/always
        let currentPath = URL(fileURLWithPath: #file)
        let projectPath = currentPath.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        cliPath = projectPath.appendingPathComponent("target/release-fast/always").path
    }

    func startDaemon() async throws -> String {
        try await runCLI(arguments: ["start"])
    }

    func stopDaemon() async throws -> String {
        try await runCLI(arguments: ["stop"])
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

    private func runCLI(arguments: [String]) async throws -> String {
        // Check if binary exists
        if !FileManager.default.fileExists(atPath: cliPath) {
            throw CLIError.binaryNotFound
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: cliPath)
        process.arguments = arguments

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