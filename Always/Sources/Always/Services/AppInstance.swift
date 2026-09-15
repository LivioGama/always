import Foundation

/// Runtime instance identity. The production app (`com.always.v3`,
/// `Always.app`) owns the unsuffixed paths forever. A development build —
/// bundle id `com.always.v3.dev`, bundle `Always Dev.app` — gets its own
/// config dir, socket, pid file, and GUI lock so it can run alongside
/// production without sharing or corrupting prod state.
///
/// Detection is path/bundle based, not env based: the GUI and its bundled
/// `always-daemon` (which self-detects via `ALWAYS_INSTANCE`/exe path on
/// the Rust side) agree on the instance with zero plumbing.
enum AppInstance {
    static let isDev: Bool = {
        if Bundle.main.bundleIdentifier?.hasSuffix(".dev") == true {
            return true
        }
        return Bundle.main.bundlePath.contains("Always Dev.app")
    }()

    /// `"-dev"` for dev, `""` for prod — applied to per-instance filenames.
    static let suffix = isDev ? "-dev" : ""

    /// `~/Library/Application Support/always[-dev]` — db, prefs, pid,
    /// voiceprint, vocabulary, gui lock.
    static var supportDir: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/always\(suffix)", isDirectory: true)
    }

    /// `~/Library/Caches/Always/always[-dev].sock` — matches the Rust
    /// `uds_server::socket_path` construction.
    static var socketPath: String {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Caches/Always/always\(suffix).sock")
            .path
    }

    static var pidPath: String {
        supportDir.appendingPathComponent("always.pid").path
    }

    static var guiLockPath: String {
        supportDir.appendingPathComponent("always.gui.lock").path
    }

    /// True when a `ps` command line belongs to THIS instance. Dev
    /// processes run from inside `Always Dev.app`; prod commands are
    /// everything else. Applied to both GUI and daemon sweeps so neither
    /// instance ever signals the other.
    static func isSameInstance(command: String) -> Bool {
        command.contains("Always Dev.app") == isDev
    }
}
