// swift-tools-version: 6.0
import PackageDescription

// WaddleKit is the UI-free core of the Apple apps: typed XMPP identities,
// the conversation timeline reducer, session stores, the coordinator that
// routes `XmppPort` events into them, and XEP-0448 file encryption and decryption. It never
// imports SwiftUI or the UniFFI bindings. Its one dependency, swift-crypto,
// re-exports CryptoKit on Apple platforms and is BoringSSL-backed elsewhere,
// so the package builds and tests on Linux as well as on Apple platforms.
let package = Package(
    name: "WaddleKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "WaddleKit", targets: ["WaddleKit"]),
    ],
    dependencies: [
        .package(url: "https://github.com/apple/swift-crypto.git", from: "4.3.1"),
    ],
    targets: [
        .target(
            name: "WaddleKit",
            dependencies: [
                .product(name: "Crypto", package: "swift-crypto"),
                .product(name: "CryptoExtras", package: "swift-crypto"),
            ],
            resources: [
                .copy("Compose/Suggestions/Resources/EmojiShortcodes.json"),
                .copy("Compose/Suggestions/Resources/EmojiShortcodes-LICENSE.txt"),
            ]
        ),
        // Test-only: the scripted port and fixtures. The apps' screen tests
        // compile these sources into their own bundle (see project.yml).
        .target(name: "WaddleKitTestSupport", dependencies: ["WaddleKit"]),
        .testTarget(
            name: "WaddleKitTests",
            dependencies: [
                "WaddleKit",
                "WaddleKitTestSupport",
                .product(name: "Crypto", package: "swift-crypto"),
                .product(name: "CryptoExtras", package: "swift-crypto"),
            ]
        ),
    ]
)
