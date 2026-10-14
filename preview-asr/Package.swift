// swift-tools-version:5.10
// always-preview-asr — the live-preview ASR helper (Parakeet EOU 160 ms on
// the Neural Engine via FluidAudio). Spawned and fed by the Rust daemon;
// see src/always/preview_asr.rs for the wire protocol. Preview text only:
// nothing this process produces is ever pasted.
import PackageDescription
let package = Package(
  name: "always-preview-asr",
  platforms: [.macOS(.v14)],
  dependencies: [.package(url: "https://github.com/FluidInference/FluidAudio.git", exact: "0.17.7")],
  targets: [
    .executableTarget(
      name: "always-preview-asr",
      dependencies: [.product(name: "FluidAudio", package: "FluidAudio")])
  ]
)
