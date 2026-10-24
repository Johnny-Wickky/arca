// swift-tools-version: 5.9
// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import PackageDescription

let package = Package(
    name: "ArcaKit",
    defaultLocalization: "en",
    platforms: [
        .iOS(.v16),
        .macOS(.v13)
    ],
    products: [
        .library(
            name: "ArcaKit",
            targets: ["ArcaKit"]
        ),
        .library(
            name: "ArcaCore",
            targets: ["ArcaCore"]
        )
    ],
    targets: [
        // C-ABI header and Clang module map wrapping libarca_ffi
        .target(
            name: "ArcaCore",
            path: "Sources/ArcaCore",
            publicHeadersPath: "include"
        ),
        // Idiomatic safe Swift SDK
        .target(
            name: "ArcaKit",
            dependencies: ["ArcaCore"],
            path: "Sources/ArcaKit"
        ),
        // Comprehensive test suite
        .testTarget(
            name: "ArcaKitTests",
            dependencies: ["ArcaKit"],
            path: "Tests/ArcaKitTests"
        )
    ]
)
