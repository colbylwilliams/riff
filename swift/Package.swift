// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Riff",
    platforms: [.iOS(.v17), .macOS(.v14), .visionOS(.v1)],
    products: [
        .library(name: "RiffCore", targets: ["RiffCore"]),
        .library(name: "RiffOpenAIRealtime", targets: ["RiffOpenAIRealtime"]),
        .library(name: "RiffAudio", targets: ["RiffAudio"]),
    ],
    targets: [
        // The shared engine. No dependencies, no platform APIs: everything that decides what a
        // prompt may contain lives here so it behaves identically everywhere.
        .target(
            name: "RiffCore",
            resources: [.copy("Resources/riff-agent.bundle.json")],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .target(
            name: "RiffOpenAIRealtime",
            dependencies: ["RiffCore"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        // Microphone capture and playback, including the audio session configuration that decides
        // whether the agent hears itself.
        .target(
            name: "RiffAudio",
            dependencies: ["RiffCore"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "RiffCoreTests",
            dependencies: ["RiffCore", "RiffOpenAIRealtime"],
            resources: [.copy("Resources")],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)
