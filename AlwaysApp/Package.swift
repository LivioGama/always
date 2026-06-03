// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "AlwaysApp",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(
            name: "AlwaysApp",
            targets: ["AlwaysApp"]
        )
    ],
    targets: [
        .executableTarget(
            name: "AlwaysApp",
            dependencies: [],
            path: "Sources"
        )
    ]
)