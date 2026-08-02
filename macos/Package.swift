// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "ClipSyncMacOS",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "ClipSyncMacOS", targets: ["ClipSyncMacOS"]),
        .executable(name: "ClipSyncDeadlockProbe", targets: ["ClipSyncDeadlockProbe"]),
    ],
    targets: [
        .binaryTarget(
            name: "clipboard_coreFFI",
            path: "Frameworks/ClipboardCore.xcframework"
        ),
        .target(
            name: "ClipboardCoreBindings",
            dependencies: ["clipboard_coreFFI"]
        ),
        .target(
            name: "ClipSyncMacOSKit",
            dependencies: ["ClipboardCoreBindings"]
        ),
        .executableTarget(
            name: "ClipSyncMacOS",
            dependencies: ["ClipSyncMacOSKit"],
            path: "Sources/ClipSyncMacOSApp",
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", "Support/Info.plist",
                ]),
            ]
        ),
        .executableTarget(name: "ClipSyncDeadlockProbe"),
        .testTarget(
            name: "ClipSyncMacOSKitTests",
            dependencies: ["ClipSyncMacOSKit"]
        ),
    ]
)
