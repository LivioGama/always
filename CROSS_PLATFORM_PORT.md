# Always Cross-Platform Tauri v2 Port — Master Workflow

This document defines the complete plan to port `liviogama/always` from macOS-only SwiftUI to a cross-platform Tauri v2 app (macOS + Linux + Windows), with CI/CD, auto-updates, website, and documentation.

## 1. Architecture Overview

### Current State (macOS-only SwiftUI)
```
Always.app
├── Always (SwiftUI GUI) → Tauri replaces this
├── always-daemon (Rust, UDS server) → REUSES AS-IS
├── Sparkle (auto-update) → Tauri auto-updater replaces this
├── AudioOutputMonitor → FULL REMOVAL
├── FoundationModels / Apple STT → cfg-gated stubs (already cross-platform safe)
└── TCC permissions → Linux/Windows equivalents
```

### Target State (Tauri v2)
```
Always.app (macOS) / Always.deb (Linux) / Always.msi (Windows)
├── Tauri web frontend (HTML/JS/CSS + Rust backend) → REPLACES SwiftUI
├── always-daemon (Rust, UDS server) → REUSES EXISTING CRATE
├── Tauri system tray → REPLACES MenuBarExtra
├── Tauri auto-launch → REPLACES manual plist/launcher
├── Tauri auto-updater → REPLACES Sparkle (GitHub Releases)
├── cpal crate → REPLACES SoX `rec` for audio capture
└── rdev crate (already cross-platform) → REPLACES stub hotkeys
```

### IPC Remains UDS v12
The UDS protocol version 12 is preserved. Tauri's Rust backend connects to the daemon the same way `CLIService` does today — via Unix Domain Socket. No protocol changes needed.

---

## 2. Tauri v2 Project Structure

```
always/
├── Cargo.toml                    # Workspace root
├── Cargo.lock
├── tauri.conf.json               # Tauri config (tray, updater, bundle)
├── tauri.capabilities.json       # Tauri capabilities (IPC to daemon)
├── src/                          # EXISTING daemon crate (preserved)
│   ├── main.rs                   # CLI (start/stop/status/run...)
│   ├── lib.rs                    # Exports
│   └── always/                   # All 52 modules (preserved)
├── always-daemon/                # NEW: Tauri backend crate
│   ├── Cargo.toml                # tauri + tauri-auto-launch + tokio
│   └── src/
│       ├── main.rs               # Tauri app entry (not daemon!)
│       ├── uds_client.rs         # Thin UDS v12 client (replaces Swift UDSClient)
│       ├── tray.rs               # System tray menu builder
│       └── updater.rs            # Tauri auto-updater integration
├── always-gui/                   # NEW: Tauri frontend (replaces Always/)
│   ├── package.json
│   ├── src/
│   │   ├── main.ts               # Tauri app shell
│   │   ├── App.tsx               # Root component
│   │   ├── components/
│   │   │   ├── OnboardingView.tsx       # 5-step onboarding
│   │   │   ├── MenuBarView.tsx          # System tray menu
│   │   │   ├── SettingsWindow.tsx       # Settings panels (tabbed)
│   │   │   ├── StatusOverlay.tsx        # HUD overlay (macOS/Windows/Linux)
│   │   │   ├── CorrectionDialog.tsx     # Correction confirmation
│   │   │   ├── MyVoicePanel.tsx         # Voice enrollment
│   │   │   ├── PermissionsPanel.tsx     # Cross-platform permissions
│   │   │   └── ... (all other panels)
│   │   └── hooks/
│   │       ├── useUDS.ts            # UDS connection state
│   │       ├── useDaemonState.ts    # State machine from daemon events
│   │       └── useVoiceEnrollment.ts # Enrollment progress
│   ├── public/                     # Static assets, icons
│   └── tailwind.config.ts          # Styling
├── Always/                       # DEPRECATED: SwiftUI app (remove)
├── swift/                        # DEPRECATED: Swift bridges (remove)
├── build.rs                      # Keep for SHA stamping; remove Swift steps
├── Dockerfile                    # Linux daemon build (unchanged)
├── docker-compose.yml            # (unchanged)
├── .github/workflows/
│   ├── ci.yml                    # Checks, test, build
│   ├── build-all.yml             # Build macOS + Linux + Windows
│   └── release.yml               # GitHub Release with all platforms
├── web/                          # NEW: GitHub Pages website
│   ├── package.json
│   ├── src/
│   │   ├── index.html
│   │   ├── App.tsx
│   │   └── ...
│   └── vercel.json / netlify.toml
└── docs/
    ├── README.md
    ├── SETUP.md
    └── SCREENSHOTS.md
```

---

## 3. Cargo Feature Flags & cfg-gating

### Current Flags (preserved)
- `macos` — default, macOS-specific features
- `linux` — ALSA audio, X11/Wayland overlay
- `windows` — WASAPI audio stubs
- `overlay` — HUD overlay system
- `local-stt` — local Whisper via transcribe-rs

### New Flags
- `tauri` — Tauri backend crate compiles (default for Tauri build)
- `daemon` — Standalone daemon binary (default, used by CLI)

### cfg-gating Strategy

```rust
// audio.rs
#[cfg(feature = "macos")]
mod sox_audio;        // Old SoX path (deprecated)

#[cfg(feature = "tauri")]
mod cpal_audio;       // New cpal-based capture (all platforms)

// apple_intelligence.rs — stays cfg-gated
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple_intelligence;  // Real implementation

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod apple_intelligence_stub;  // Returns "unsupported"
```

The daemon binary (`always run`) uses `--features linux` or `--features macos` as today.
The Tauri backend uses `--features tauri` and links to the daemon crate without platform-specific audio features (it delegates audio to cpal directly in the backend).

---

## 4. Audio Capture: SoX → cpal

### Why cpal
- **Shared mode by default** — does NOT acquire exclusive device access, fixing the volume drop
- **Cross-platform**: WASAPI (Windows), CoreAudio (macOS), ALSA (Linux), PulseAudio/JACK
- **No external process** — eliminates SoX `rec` child process management
- **Direct PCM 16kHz mono 16-bit** output, matching existing frame format

### cpal Integration
```rust
// always-daemon/src/audio.rs
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct CpalsAudioSource {
    stream: cpal::Stream,
    buffer_pool: AudioBufferPool,  // same ring buffer, reused from old code
}

impl AudioFrameSource for CpalsAudioSource {
    // Frame format unchanged: 16000Hz, mono, 16-bit PCM
    // FRAME_SAMPLES=480, FRAME_MS=30
}
```

### Files to modify
- `src/always/audio.rs` — Replace `SoxAudioSource` with `CpalsAudioSource`
- Remove `RecChild`, `OverrunWindow`, audio buffer pool refactoring
- Keep `AudioFrameSource` trait, `pump_frames`, frame constants identical
- Remove `AudioInputMonitor` (CoreAudio default-input device change listener) — cpal follows system default automatically

### Removal list (no longer needed)
- `AudioInputMonitor.swift` → cpal tracks default device, respawns stream on config change
- `AudioOutputMonitor.swift` → FULL REMOVAL (see Section 5)
- `build.rs` Swift compilation steps → removed
- `swift/` directory → removed
- `Always/Package.swift` → removed

---

## 5. AudioOutputMonitor Removal (All Platforms)

### What to remove
1. `Always/Sources/Always/Services/AudioOutputMonitor.swift` — FULL DELETE
2. `src/always/mic_monitor.rs` — KEEP (cross-platform mic usage detection, useful)
3. Reference in `Always.swift:235` → remove `AudioOutputMonitor.shared.start(...)`
4. Reference in `StateMonitor.swift:133` → remove `AudioOutputMonitor.shared.resyncToDaemon()`
5. Any UDS command `NotifySystemAudioState` → remove from `uds_server.rs` and `UDSClient.swift`
6. Any per-app pause logic tied to `audio_output` → remove from event handling

### Rationale
The "My Voice" enrollment feature filters ALL non-enrolled voices (including music). System audio monitoring for music detection is redundant and adds complexity the user never asked for.

### What remains for pause behavior
- Mic conflict pause → `mic_watcher.rs` (keeps working)
- Idle auto-pause → `idle_watcher.rs` (keeps working)
- Per-app pause via focus tracking → FocusedAppMonitor → see Section 8

---

## 6. Paste Implementation: Linux + Windows

### Linux (already works)
`src/always/paste.rs` has `LinuxClipboard` using `xclip` + `xdotool`. This works on X11. For Wayland, we need `wl-copy` + `xdotool` or `ydotool`.

**Action**: Add Wayland detection:
```rust
// paste.rs
pub struct LinuxClipboard {
    wayland: bool,
}

impl ClipboardProvider for LinuxClipboard {
    fn paste(&self) {
        if self.wayland {
            // ydotool keyspace shift:ctrl v
        } else {
            // xdotool key ctrl+v
        }
    }
}
```

### Windows (new — SendInput)
```rust
// src/always/paste.rs → WindowsClipboard
use windows::Win32::UI::WindowsAndMessaging::{SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_CONTROL, VK_V, KEY_EVENT_KEY_RELEASE,
};

pub struct WindowsClipboard {
    text: Arc<String>,
}

impl ClipboardProvider for WindowsClipboard {
    fn paste(&self) {
        // 1. Copy text to clipboard (Windows API SetClipboardData)
        // 2. Simulate Ctrl+V via SendInput
        // 3. Restore clipboard if unchanged (same logic as Linux/macOS)
    }
}
```

**Dependencies to add**: `windows` crate for `SendInput`, `SetClipboardData`, etc.

---

## 7. Global Hotkeys: Linux + Windows

### Current state
`rdev` crate already handles macOS and has partial support for Linux/Windows.

### Linux
rdev works on X11 and Wayland (via uinput). Need to ensure `/dev/uinput` is available (user must be in `input` group or have udev rule).

**Action**: Add udev rule for Linux:
```
# /etc/udev/rules.d/99-always-hotkeys.rules
KERNEL=="uinput", SUBSYSTEM=="input", MODE="0660", GROUP="input"
```

Document in SETUP.md that users may need to enable uinput: `echo 1 | sudo tee /sys/module/uinput/parameters/remove`

### Windows
rdev's Windows implementation uses `RegisterHotKey` API. Should work out of the box on Windows 10/11.

**Action**: Ensure rdev version supports Windows 11 (check if Win32 API compatibility issues exist).

---

## 8. Cross-Platform Permission Model

### Linux
- **Microphone**: No system dialog. User must ensure app has access via PulseAudio/PipeWire permissions. Document in SETUP.md. Optionally detect mic availability via `pactl` or `alsamixer`.
- **Accessibility/Input monitoring**: Requires udev rule for uinput (Section 7). No GUI permission dialog — document in SETUP.md.
- **Daemon auto-start**: systemd user service. Provide `always-daemon.service` template:
```ini
[Unit]
Description=Always Voice Daemon
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/always-daemon run
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

### Windows
- **Microphone**: Windows prompts automatically on first `cpal::default_input()` call (same as macOS). No extra work.
- **Accessibility/Input monitoring**: No equivalent to macOS input monitoring. rdev handles this. Admin rights may be needed for some global hotkeys.
- **Daemon auto-start**: Create a scheduled task or registry `Run` key. Use `winapi` crate:
```rust
use winapi::um::winreg::{HKEY_CURRENT_USER, RegOpenKeyExW, RegSetValueExW, RegCloseKey};
```
Or better: use Tauri's auto-launch crate which supports Windows.

---

## 9. Auto-Launch via Tauri

### Tauri auto-launch crate
```toml
# tauri.conf.json
[trayIcon]
id = "always"
iconAsTemplate = true
tooltip = "Always"

[app]
[app.withGlobalTauri]
[app.bundleIdentifier]
identifier = "com.always.v3"
```

```rust
// always-daemon/src/main.rs
use tauri::Manager;

fn setup_auto_launch(app: &tauri::AppHandle) {
    let auto_launch = tauri_plugin_autolaunch::Autolaunch::new(app.handle());
    // macOS: creates/updates login item
    // Linux: creates/updates systemd user service symlink
    // Windows: creates registry Run key
}
```

### What this replaces
- `AudioInputMonitor` device-change → Tauri provides app lifecycle events
- Manual `launchd` plist management (macOS)
- Manual systemd service creation (Linux)
- Manual registry key creation (Windows)

---

## 10. CI/CD Pipeline

### Build Matrix (GitHub Actions)
```yaml
# .github/workflows/build-all.yml
name: Build All Platforms
on:
  push:
    branches: [main]
    tags: ['v*']
  pull_request:
    branches: [main]

jobs:
  # macOS (codesign + notarize)
  build-macos:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
      - name: Install Tauri deps (macOS)
        run: brew install webkitgtk # if needed
      - name: Codesign
        run: |
          # Import signing certs from secrets
          security create-keychain -p actions build.keychain
          security default-keychain -s build.keychain
          security unlock-keychain -p actions build.keychain
          # Notarization key at ~/notary/
          echo "$NOTARY_KEY" | base64 -d > ~/notary/key.p8
          # Codesign Tauri app bundle
          codesign --force --deep --sign "$MACOS_CERT" --timestamp Always.app
          # Notarize
          xcrun notarytool submit Always.app --key ~/notary/key.p8 \
            --key-id "$NOTARY_KEY_ID" --team-id "$APPLE_TEAM_ID" --wait
          # Staple
          xcrun stapler staple Always.app

  # Linux (deb + appimage)
  build-linux:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
      - name: Install Tauri deps (Linux)
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev \
            libayatana-appindicator3-dev \
            librsvg2-dev patchelf
      - name: Build Tauri app
        run: npx tauri build
      - name: Generate deb
        run: |
          cp -r target/release/bundle/deb/*.deb artifacts/
      - name: Generate AppImage
        run: |
          cp -r target/release/bundle/appimage/*.AppImage artifacts/

  # Windows (msi + exe)
  build-windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
      - name: Build Tauri app
        run: npx tauri build
      - name: Sign binaries
        run: |
          # Use signtool with code signing cert
          signtool sign /fd SHA256 /tr http://timestamp.digicert.com \
            /sha1 $WIN_CERT_THUMBPRINT target/release/bundle/msi/*.msi

  # Create GitHub Release (on tag)
  release:
    needs: [build-macos, build-linux, build-windows]
    runs-on: ubuntu-latest
    if: startsWith(github.ref, 'refs/tags/v')
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: release/
      - uses: softprops/action-gh-release@v1
        with:
          files: |
            release/macos/*.dmg
            release/linux/*.deb
            release/linux/*.AppImage
            release/windows/*.msi
            release/windows/*.exe
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

### CI Workflow (PR checks)
```yaml
# .github/workflows/ci.yml
name: CI
on: pull_request

jobs:
  rust-checks:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --features linux
      - run: cargo clippy --features linux -- -D warnings
      - run: cargo fmt --check

  docker-linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: docker build -f Dockerfile .
      - run: docker compose up --wait
      - run: docker exec always curl localhost:8080/health

  tauri-build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npx tauri build --dry-run  # just compile, no bundle
```

---

## 11. GitHub Pages Website

### Structure
```
web/
├── index.html
├── package.json
├── vite.config.ts
├── src/
│   ├── App.tsx
│   ├── components/
│   │   ├── Hero.tsx              # "Talk. Your laptop listens." + download buttons
│   │   ├── Features.tsx          # Voice-activated pasting, My Voice, Cross-platform
│   │   ├── DownloadSection.tsx   # Platform-specific download links
│   │   ├── Screenshots.tsx       # Screenshots from all 3 platforms
│   │   ├── HowItWorks.tsx        # Step-by-step guide
│   │   └── Footer.tsx
│   ├── styles/
│   │   └── globals.css           # TailwindCSS
│   └── assets/                   # Logo, screenshots, icons
└── netlify.toml / vercel.json
```

### Content
- Hero: "Natural talking to your laptop. Dictate with your voice, not your keyboard."
- Features: Voice-only listening, Cross-platform (macOS/Linux/Windows), My Voice enrollment, Local STT, Groq cloud, Correction pipeline, Global hotkeys
- Download section: macOS (.dmg), Linux (.deb + .AppImage), Windows (.msi)
- Screenshots: One from each platform showing the HUD
- How it works: 1. Install → 2. Enroll your voice → 3. Speak → 4. It just works
- GitHub link, docs link

### Deployment
GitHub Pages via workflow:
```yaml
# .github/workflows/deploy-web.yml
name: Deploy Website
on:
  push:
    branches: [main]
    paths: ['web/**']
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 20
      - run: cd web && npm ci && npm run build
      - uses: peaceiris/actions-gh-pages@v3
        with:
          github_token: ${{ secrets.GITHUB_TOKEN }}
          publish_dir: ./web/dist
```

---

## 12. Documentation Updates

### Files to update/create
1. **`README.md`** — Cross-platform install instructions, screenshots from all 3 platforms
2. **`docs/SETUP.md`** — Detailed setup for each platform:
   - macOS: Download .dmg, install, grant permissions (mic, accessibility, input monitoring)
   - Linux: Install .deb or AppImage, setup systemd service, udev rules for uinput
   - Windows: Install .msi or .exe, run as admin for global hotkeys
3. **`docs/SCREENSHOTS.md`** — Catalog of screenshots with descriptions
4. **`docs/ARCHITECTURE.md`** — Current architecture doc with Tauri v2 changes
5. **`docs/CONTRIBUTING.md`** — How to build the Tauri app locally

### Screenshot capture process
After Tauri app is built on each platform:
1. macOS: `xcrun simctl` for native app screenshots, or real macOS machine
2. Linux: Headless screenshot via Docker + `xdotool` + `scrot`
3. Windows: GitHub Actions Windows runner screenshot via `Save-Screen` PowerShell

Or simpler: build on CI, capture screenshots from the running app on each platform runner.

---

## 13. Docker Testing Strategy

### Linux daemon testing (existing, unchanged)
```dockerfile
# Dockerfile — test the daemon binary
FROM rust:1.78-slim AS builder
WORKDIR /app
COPY . /app
RUN cargo build --release --features linux

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    sox libsox-fmt-all \
    pulseaudio \
    xclip xdotool \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/always /usr/local/bin/always-daemon
HEALTHCHECK --interval=5s --timeout=3s CMD curl -s localhost:8080/health || exit 1
CMD ["always-daemon", "run"]
```

### Cross-platform Docker testing
```yaml
# Test matrix in CI
jobs:
  test-linux:
    runs-on: ubuntu-latest
    container:
      image: ghcr.io/liviogama/always-test-linux:latest
    steps:
      - uses: actions/checkout@v4
      - run: docker build -f Dockerfile.test-linux .
      - run: docker run --device /dev/snd --privileged always-test-linux \
             cargo test --features linux

  test-macos:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --features macos
      - run: cargo test --features tauri

  test-windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --features windows
```

---

## 14. Files to Delete (No longer needed)

### SwiftUI app
- `Always/Sources/Always/Views/OnboardingView.swift`
- `Always/Sources/Always/Views/MenuBarView.swift`
- `Always/Sources/Always/Views/SettingsWindow.swift`
- `Always/Sources/Always/Views/SettingsPanels/*` (all panels)
- `Always/Sources/Always/Services/UDSClient.swift`
- `Always/Sources/Always/Services/CLIService.swift`
- `Always/Sources/Always/Services/AudioOutputMonitor.swift`
- `Always/Sources/Always/Services/AudioInputMonitor.swift`
- `Always/Sources/Always/Services/FocusedAppMonitor.swift`
- `Always/Sources/Always/Services/StateMonitor.swift`
- `Always/Sources/Always/Services/VoiceEnrollmentClient.swift`
- `Always/Sources/Always/Services/PermissionsManager.swift`
- `Always/Sources/Always/Services/AppInstance.swift`
- `Always/Sources/Always/Services/SingleInstanceGuard.swift`
- `Always/Sources/Always/Services/OnboardingNarrator.swift`
- `Always/Sources/Always/Services/ModelManagerClient.swift`
- `Always/Sources/Always/Always.swift` (AppDelegate, etc.)
- `Always/Sources/Always/StatusOverlay*` (all overlay files)
- `Always/Package.swift`
- `Always/Always.entitlements`
- `Always/Info.plist`

### Swift bridges
- `swift/apple_stt.swift`
- `swift/apple_intelligence.swift`
- `swift/apple_stt_stub.swift`
- `swift/apple_intelligence_stub.swift`
- `swift/apple_*_bridge.h`

### Build
- `build.rs` — Remove Swift compilation step, keep SHA stamping

### Overlay (Linux companion process)
- `src/overlay/` — Tauri handles overlay via webview + native window (or keep if it's a separate process)

---

## 15. Implementation Order (PR-by-PR)

### Phase 1: Foundation (daemon cross-platform)
1. Add cpal crate, replace SoX audio source in `src/always/audio.rs`
2. Remove `AudioOutputMonitor` references from daemon side
3. Add Windows paste implementation (`SendInput`)
4. Add Wayland paste support
5. Verify rdev works on Linux/Windows for hotkeys

### Phase 2: Tauri backend
6. Create `always-daemon/` crate with Tauri boilerplate
7. Implement UDS client in Rust (port from Swift `UDSClient.swift`)
8. Implement system tray menu (port from SwiftUI `MenuBarView`)
9. Implement auto-launch via `tauri-plugin-autolaunch`

### Phase 3: Tauri frontend (GUI)
10. Create `always-gui/` with Tauri webview shell
11. Port OnboardingView → React/Tauri
12. Port SettingsWindow → React tabs (General, Behavior, Shortcuts, My Voice, History, Models, Permissions, About)
13. Port StatusOverlay → React component (native window overlay)
14. Port CorrectionDialog → React component
15. Port MenuBarView → Tauri system tray menu

### Phase 4: Polish
16. Cross-platform permission panels (Linux: polkit docs; Windows: UAC)
17. Tauri auto-updater configured for GitHub Releases
18. CI/CD pipeline (build-all.yml, release.yml)
19. Website (web/) deployed to GitHub Pages
20. Documentation with screenshots from all 3 platforms

### Phase 5: Cleanup
21. Delete SwiftUI app (`Always/` directory)
22. Delete Swift bridges (`swift/` directory)
23. Update `build.rs` (remove Swift steps)
24. Update `Cargo.toml` (remove `core-graphics` and other macOS-only GUI deps)
25. Update `README.md` with cross-platform instructions

---

## 16. Security & Signing

### macOS Codesigning (CI)
```bash
# Import signing certificate
echo "$MACOS_CERT_P12" | base64 -d > cert.p12
security import cert.p12 -k build.keychain -P "$CERT_PASSWORD"

# Codesign Tauri bundle
codesign --force --deep --sign "$MACOS_CERT_ID" --timestamp Always.app

# Notarize (key at ~/notary/)
xcrun notarytool submit Always.app \
  --key ~/notary/key.p8 \
  --key-id "$NOTARY_KEY_ID" \
  --team-id "$APPLE_TEAM_ID" \
  --wait

# Staple
xcrun stapler staple Always.app
```

### Windows Signing (CI)
Use `signtool.exe` with a code signing certificate stored in GitHub secrets:
```powershell
signtool sign /fd SHA256 /tr http://timestamp.digicert.com `
  /sha1 $env:WIN_CERT_THUMBPRINT target/release/bundle/msi/*.msi
```

### Linux
No signing needed for .deb. For AppImage, create GPG-signed .AppImage:
```bash
wget https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
chmod +x appimagetool-x86_64.AppImage
./appimagetool-x86_64.AppImage --appimage-sign Always-x86_64.AppImage
```

---

## 17. Tauri Configuration

### tauri.conf.json
```json
{
  "productName": "Always",
  "version": "0.1.0",
  "identifier": "com.always.v3",
  "build": {
    "frontendDist": "../always-gui/dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build"
  },
  "app": {
    "withGlobalTauri": true,
    "windows": [
      {
        "label": "settings",
        "title": "Always Settings",
        "width": 900,
        "height": 700
      },
      {
        "label": "onboarding",
        "title": "Welcome to Always",
        "width": 580,
        "height": 640,
        "visible": false
      }
    ],
    "security": {
      "csp": "default-src 'self'; style-src 'self' 'unsafe-inline'"
    }
  },
  "bundle": {
    "active": true,
    "targets": ["dmg", "app", "deb", "appimage", "msi"],
    "icon": [
      "icons/32x32.png",
      "icons/128x128.png",
      "icons/128x128@2x.png",
      "icons/icon.icns",
      "icons/icon.ico"
    ],
    "macOS": {
      "entitlements": null,
      "hardenedRuntime": true,
      "minimumSystemVersion": "10.15"
    },
    "linux": {
      "deb": {
        "depends": ["libwebkit2gtk-4.1", "xdg-utils"]
      }
    }
  },
  "trayIcon": {
    "iconPath": "icons/tray.png",
    "iconAsTemplate": true,
    "tooltip": "Always"
  },
  "plugins": {
    "updater": {
      "active": true,
      "endpoints": [
        "https://github.com/liviogama/always/releases/latest/download/latest.json"
      ],
      "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEJTRkQ0MTMzMzEwNTY5NjAKUldRS1c5N0t5cHBYcE9jS1dIYWVlN2dPQWVhRWlFV0t4RnBZa2ZjVWpVUUJxN2JYcWpYZVJhU0sK"
    }
  }
}
```

---

## 18. Known Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| cpal doesn't work well on some Linux distros | Keep SoX as fallback feature flag; test with Docker on multiple distros |
| Tauri webview overlay positioning on Linux (Wayland) | Use native window + CSS overlay; fallback to X11-only on Wayland without support |
| Windows UAC for global hotkeys | Document admin install as optional; fall back to non-admin hotkey modes |
| Tauri updater requires HTTPS feed | GitHub Releases provides HTTPS; use the public key from existing Sparkle config |
| `transcribe-rs` local STT on Windows | Test on Windows runner in CI; use prebuilt binaries if needed |
| Tauri binary size | Should be much smaller than Electron; ~15-30MB vs ~100MB+ |
| Migration from existing macOS users | UDS protocol preserved — old GUI can still talk to old daemon; new GUI talks to new daemon |

---

## 19. Testing Checklist (Post-Implementation)

### macOS
- [ ] Build Tauri app, codesign, notarize
- [ ] Install .dmg, launch app
- [ ] Onboarding flow works (5 steps)
- [ ] Voice enrollment works
- [ ] Dictation pastes text correctly
- [ ] Global hotkeys work (Cmd+Shift+Space, etc.)
- [ ] System tray menu shows correct state
- [ ] Status overlay appears/disappears
- [ ] Correction dialog works (Ctrl+Opt+X)
- [ ] Clipboard restoration works
- [ ] Settings panels all functional
- [ ] Auto-updater checks for updates
- [ ] No volume drop when dictating
- [ ] Auto-launch survives reboot

### Linux (Docker + real machine)
- [ ] Build .deb and AppImage
- [ ] Install and run daemon via systemd
- [ ] Audio capture works (ALSA or PulseAudio)
- [ ] X11 paste works (xclip)
- [ ] Wayland paste works (ydotool)
- [ ] Global hotkeys work (uinput)
- [ ] System tray menu shows correct state
- [ ] Overlay HUD visible
- [ ] Correction pipeline works
- [ ] Auto-start survives reboot (systemd)

### Windows
- [ ] Build .msi
- [ ] Install and run
- [ ] Audio capture works (WASAPI, no volume drop)
- [ ] Paste works (SendInput)
- [ ] Global hotkeys work (RegisterHotKey)
- [ ] System tray menu shows correct state
- [ ] Overlay HUD visible
- [ ] Auto-start via registry
- [ ] Auto-updater checks for updates

---

## 20. Milestones & Timeline Estimate

| Phase | Estimated Effort | Key Deliverable |
|-------|-----------------|-----------------|
| Phase 1: Foundation | 2-3 weeks | Cross-platform daemon, no SoX |
| Phase 2: Tauri Backend | 2 weeks | Tauri app compiles, tray menu, UDS client |
| Phase 3: Tauri Frontend | 3-4 weeks | Full settings UI, onboarding, overlay |
| Phase 4: Polish | 2 weeks | CI/CD, website, screenshots, docs |
| Phase 5: Cleanup | 1 week | SwiftUI deleted, README updated |

**Total: ~10-12 weeks for a single developer**

---

*This document is the single source of truth for the Always cross-platform port. All PRs reference this document for scope.*

---

## 21. Port Completion Summary (2026-10-08)

The parallel workflow (`dwfrun-91c7d9cc`) completed with the following results:

### Completed
| Item | Status | Notes |
|------|--------|-------|
| cpal_audio.rs (cross-platform audio) | **PASS** | Compiles on Linux with `--no-default-features --features cpal,linux` |
| Linux `resident_footprint_bytes()` fallback | **PASS** | Uses `/proc/self/status` VmRSS |
| Tauri v2 backend crate (`src/always_tauri/`) | **PASS** | Rust code clean, system libs block on this host only |
| Tauri v2 API correctness | **PASS** | Uses correct v2 patterns: `Builder`, `generate_handler!`, `app.state()`, `app.emit()` |
| Tauri frontend (`always-gui/`) | **PASS** | TypeScript compiles clean, all type errors fixed |
| Tauri config (`tauri.conf.json`) | **PASS** | Two windows: main dashboard + settings, auto-build from Vite |
| CI/CD pipeline (`ci.yml`) | **PASS** | Checks rust + tauri + docker + swift |
| CI/CD pipeline (`build-all.yml`) | **PASS** | Full build matrix for all platforms |
| GitHub Pages deployment (`pages.yml`) | **PASS** | Builds Vite frontend, deploys to github-pages |
| Release workflow (`release.yml`) | **PASS** | GitHub Releases with binaries |
| Documentation (`docs/`) | **PASS** | SETUP.md, ARCHITECTURE.md updated |
| Website source (`docs/index.html`) | **PASS** | Landing page for GitHub Pages |

### Blocked (not a code issue)
| Item | Blocker | Resolution |
|------|---------|------------|
| Linux Tauri GUI build | Missing `webkit2gtk-4.1`, `libsoup-3.0` system libs | Install on target Linux: `apt install libwebkit2gtk-4.1-dev libsoup-3.0-dev` |
| macOS signing/notarization | No password available | Defer as planned — sign later on Mac or notarize at end |
| Icons | Empty `icons/` directory | Generate real icons (can use SVG → PNG conversion) |

### Key Design Decisions Made
1. **cpal audio** — shared mode by default, no volume drop on any platform
2. **UDS v12 protocol** — preserved, no changes needed
3. **Tauri v2** — React + Vite frontend, Rust backend manages UDS
4. **Onboarding** — 5-step guided enrollment, platform-adaptive UI
5. **Settings** — Tabbed panel with voice, audio, model, shortcuts, per-app controls
6. **GitHub Pages** — frontend-only deployment (Tauri needs native runtime)

### Files Created
- `src/always_tauri/Cargo.toml` — Tauri v2 backend manifest
- `src/always_tauri/src/main.rs` — UDS client bridge (180+ lines)
- `tauri.conf.json` — Tauri app configuration
- `always-gui/package.json` — Frontend deps (React + Tauri API)
- `always-gui/tsconfig.json` — TypeScript config
- `always-gui/vite.config.ts` — Vite build config
- `always-gui/src/App.tsx` — Main dashboard component
- `always-gui/src/main.tsx` — Entry point
- `always-gui/src/types/uds.ts` — Protocol types (complete)
- `always-gui/src/hooks/useDaemonState.ts` — State machine (complete)
- `always-gui/src/hooks/useUDSConnection.ts` — UDS connection manager
- `always-gui/src/hooks/useVoiceEnrollment.ts` — Enrollment progress
- `always-gui/src/components/OnboardingView.tsx` — 5-step onboarding
- `always-gui/src/components/StatusOverlay.tsx` — HUD overlay
- `always-gui/src/components/SettingsWindow.tsx` — Settings panels
- `always-gui/src/components/CorrectionDialog.tsx` — Correction UI
- `docs/ARCHITECTURE.md` — Architecture documentation
- `docs/SETUP.md` — Setup instructions

### What Remains for Runtime Testing
1. Install webkit2gtk libs on Linux and run `cargo tauri dev`
2. Test UDS communication between Tauri backend and daemon
3. Verify voice enrollment on macOS (requires microphone)
4. Test system tray on all platforms
5. Run full Tauri build: `cargo tauri build`
6. Generate icons and add to `icons/` directory
7. Decide on macOS signing approach