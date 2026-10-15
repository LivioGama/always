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
    targets: [
        .executableTarget(
            name: "Always",
            path: "Sources"
        ),
        .testTarget(
            name: "AlwaysTests",
            dependencies: ["Always"],
            path: "Tests"
        )
    ]
)
