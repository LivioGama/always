# Contributing to Always

Thank you for your interest in contributing to Always! This document provides guidelines for contributing to the project.

## Development Setup

### Prerequisites
- macOS 14+ (Sonoma or later) on Apple Silicon
- Rust 1.70 or later
- Swift 5.9 or later
- Xcode 15 or later (for Swift app development)
- SoX (via Homebrew: `brew install sox`)

### Building

```bash
# Build the Rust daemon
cargo build --release

# Build the Swift menu bar app
cd AlwaysApp
swift build
```

### Running Tests

```bash
# Run Rust tests
cargo test

# Run all checks (formatting, clippy, tests)
cargo fmt --check
cargo clippy --release -- -D warnings
cargo test
```

## Code Style

### Rust
- Follow standard Rust formatting: `cargo fmt`
- Use `cargo clippy` for linting
- All warnings must be addressed before merging
- Add tests for new functionality
- Document public APIs with rustdoc comments

### Swift
- Follow standard Swift formatting
- Use SwiftUI for UI components
- Prefer `os.Logger` for logging over `print()`
- Use `@MainActor` for UI updates

## Submitting Changes

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

### Pull Request Guidelines
- All CI checks must pass
- Describe your changes in the PR description
- Link related issues
- Update documentation if needed
- Add tests for new functionality

## Project Structure

```
.
├── AlwaysApp/              # SwiftUI menu bar application
│   ├── Sources/
│   └── Package.swift
├── src/
│   ├── always/            # Core daemon modules
│   │   ├── audio.rs
│   │   ├── vad.rs
│   │   ├── event_loop.rs
│   │   └── ...
│   ├── db.rs              # SQLite database
│   ├── main.rs            # CLI entry point
│   └── ...
├── docs/                  # Documentation
├── examples/              # Example programs
└── Cargo.toml
```

## Reporting Issues

When reporting bugs, please include:
- macOS version
- Always version
- Steps to reproduce
- Expected vs actual behavior
- Relevant logs (with sensitive data redacted)

## License

By contributing, you agree that your contributions will be licensed under the Apache-2.0 License.
