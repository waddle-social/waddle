// swift-tools-version: 6.0
import PackageDescription

// WaddleKit is the Foundation-only core of the Apple apps: typed XMPP
// identities, the conversation timeline reducer, session stores, and the
// coordinator that routes `XmppPort` events into them. It never imports
// SwiftUI or the UniFFI bindings, so it builds and tests on Linux as well
// as on Apple platforms.
let package = Package(
    name: "WaddleKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "WaddleKit", targets: ["WaddleKit"]),
    ],
    targets: [
        .target(name: "WaddleKit"),
        .testTarget(name: "WaddleKitTests", dependencies: ["WaddleKit"]),
    ]
)
