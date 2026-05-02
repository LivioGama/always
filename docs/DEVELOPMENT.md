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
```

## Dependencies

- **Rust** 1.70+
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
4. Run `cargo test`
5. Build with `cargo build --release`
6. Submit a pull request
