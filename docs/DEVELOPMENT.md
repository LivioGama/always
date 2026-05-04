# 🛠️ Development

## Building

```bash
# Fast development builds
cargo build --profile release-fast

# Production builds
cargo build --release
```

## Testing

```bash
# Run all tests
cargo test

# Test specific module
cargo test filter
cargo test audio

# Swift app tests
cd AlwaysApp && swift test

# Quality gates used by CI
cargo fmt --check
cargo clippy --release -- -D warnings
```

## Dependencies

- **Rust** stable with edition 2024 support
- **Swift** 5.9+ / macOS 14+
- **SoX** (audio recording)
  - macOS: `brew install sox`
  - Ubuntu/Debian: `apt install sox`
  - Arch: `pacman -S sox`
  - Windows: Download from [sox.sourceforge.net](http://sox.sourceforge.net/)
- **Groq API Key** (free at groq.com)

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make changes with tests
4. Run `cargo fmt --check`, `cargo clippy --release -- -D warnings`, and `cargo test`
5. Run `cd AlwaysApp && swift build && swift test` for UI-adjacent changes
6. Submit a pull request

## Runtime Notes

- The Swift app communicates with the daemon through the UDS server in
  `src/always/uds_server.rs`.
- Settings writes use the CLI/config layer; shortcut changes take effect after a
  daemon restart.
- Avoid requiring a live Groq key in tests. Use local/mocked HTTP paths for STT
  coverage.
