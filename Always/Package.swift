// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Always",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(
            name: "Always",
            targets: ["Always"]
        )
    ],
    dependencies: [
        // Sparkle drives in-app auto-update against the appcast.xml emitted
        // by the release workflow. EdDSA signing keys are documented in
        // docs/RELEASE.md; the public half lives in Info.plist (`SUPublicEDKey`).
        .package(url: "https://github.com/sparkle-project/Sparkle.git", from: "2.6.0")
    ],
    targets: [
        .executableTarget(
            name: "Always",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle")
            ],
            path: "Sources"
        ),
        .testTarget(
            name: "AlwaysTests",
            dependencies: ["Always"],
            path: "Tests"
        )
    ]
)
