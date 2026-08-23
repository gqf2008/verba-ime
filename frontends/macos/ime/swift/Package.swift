// swift-tools-version:5.7
import PackageDescription

let package = Package(
    name: "VerbaIMK",
    platforms: [.macOS(.v11)],
    products: [
        .library(name: "VerbaIMK", targets: ["VerbaIMK"])
    ],
    targets: [
        .target(
            name: "VerbaIMK",
            linkerSettings: [
                .linkedFramework("Cocoa"),
                .linkedFramework("InputMethodKit")
            ]
        )
    ]
)
