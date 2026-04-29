import Foundation

struct DaemonStatus: Codable {
    let isRunning: Bool
    let pid: Int?
    let logPath: String?

    static func fromCLI(output: String) -> DaemonStatus? {
        if output.contains("Always-on daemon running") {
            // Extract PID
            if let pidRange = output.range(of: #"pid (\d+)"#, options: .regularExpression) {
                let pidString = output[pidRange].replacingOccurrences(of: "pid ", with: "")
                if let pid = Int(pidString) {
                    return DaemonStatus(isRunning: true, pid: pid, logPath: nil)
                }
            }
            return DaemonStatus(isRunning: true, pid: nil, logPath: nil)
        }
        return DaemonStatus(isRunning: false, pid: nil, logPath: nil)
    }
}