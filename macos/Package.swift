// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "ClipSyncMacOS",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "ClipSyncMacOS", targets: ["ClipSyncMacOS"]),
    ],
    targets: [
        .executableTarget(
            name: "ClipSyncMacOS",
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", "Support/Info.plist",
                ]),
            ]
        ),
    ]
)
