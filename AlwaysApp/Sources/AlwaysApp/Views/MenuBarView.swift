import SwiftUI
import AppKit

struct MenuBarView: View {
    @StateObject private var cliService = CLIService()
    @State private var status: DaemonStatus?
    @State private var config: Config?
    @State private var isLoading = false
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Status section
            HStack {
                Image(systemName: status?.isRunning == true ? "mic.fill" : "mic.slash.fill")
                    .foregroundColor(status?.isRunning == true ? .green : .red)
                Text(status?.isRunning == true ? "Running" : "Stopped")
                    .font(.headline)
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)

            Divider()

            // Quick actions
            Button(action: toggleDaemon) {
                Label(
                    status?.isRunning == true ? "Stop Daemon" : "Start Daemon",
                    systemImage: status?.isRunning == true ? "stop.circle" : "play.circle"
                )
            }
            .disabled(isLoading)
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Button(action: openSettings) {
                Label("Settings", systemImage: "gear")
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Divider()

            Button("Quit") {
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
        }
        .frame(width: 220)
        .onAppear {
            Task {
                await refreshStatus()
            }
        }
    }

    private func toggleDaemon() {
        isLoading = true
        Task {
            do {
                if status?.isRunning == true {
                    _ = try await cliService.stopDaemon()
                } else {
                    _ = try await cliService.startDaemon()
                }
                await refreshStatus()
            } catch {
                print("Error toggling daemon: \(error)")
            }
            isLoading = false
        }
    }

    private func openSettings() {
        openWindow(id: "settings")
        
        // Bring the settings window to front
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            NSApp.activate(ignoringOtherApps: true)
            if let window = NSApp.windows.first(where: { $0.title == "Always Settings" }) {
                window.makeKeyAndOrderFront(nil)
                window.orderFrontRegardless()
            }
        }
    }

    private func refreshStatus() async {
        do {
            status = try await cliService.getStatus()
            config = try await cliService.getConfig()
        } catch {
            print("Error refreshing status: \(error)")
            status = DaemonStatus(isRunning: false, pid: nil, logPath: nil)
        }
    }
}
