# Deprecated

## Swift GUI (Always/) — DEPRECATED

The Swift-based macOS GUI at `Always/` is **deprecated** as of 2026-10-08.

### What it was

The Swift GUI provided a native macOS application with settings panels,
onboarding flow, listening indicator overlay, menu bar status, and
preferences management. It communicated with the Rust daemon via Unix
Domain Sockets (UDS).

### Why it's deprecated

The project has moved to a Tauri-based architecture (`crates/always_tauri/`)
which provides a cross-platform web UI with Rust backend, eliminating the
need for a separate Swift application. The Tauri approach is simpler to
maintain, supports Linux and Windows natively, and uses the same daemon
UDS protocol.

### What to use instead

- **macOS**: Use the Tauri app (`crates/always_tauri/`)
- **Linux**: The daemon (`src/main.rs`) runs headless; use the Tauri app
  via `cargo run --package always_tauri` or a Linux Tauri build
- **Windows**: Same as Linux — daemon + Tauri GUI

### Files being removed or ignored

- `Always/` — Swift GUI project (no longer maintained)
- `Always/Sources/Always/` — Swift source code
- `Always/Tests/` — Swift tests
- `swift/` — Apple Intelligence / Apple STT Swift bridges (used by
  `build.rs`, now removed)
- `build.rs` Swift compilation functions (removed in 2026-10-08 cleanup)

The `Info.plist` embed in `build.rs` is preserved for TCC permissions
on macOS, but it now reads from `Always/Info.plist` only when present.

### Migration

If you have customizations in the Swift GUI that you need to port to
the Tauri frontend, see the Tauri source in `crates/always_tauri/`.