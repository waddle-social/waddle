import SwiftUI

extension View {
    /// Applies a glass effect on iOS/macOS 26+; falls back to `.regularMaterial` on earlier OS versions.
    @ViewBuilder
    func waddleGlass<S: Shape>(in shape: S) -> some View {
        if #available(iOS 26.0, macOS 26.0, *) {
            self.glassEffect(in: shape)
        } else {
            self.background(.regularMaterial).clipShape(shape)
        }
    }

    /// Applies an interactive glass effect on iOS/macOS 26+; falls back to `.regularMaterial`.
    @ViewBuilder
    func waddleInteractiveGlass<S: Shape>(in shape: S) -> some View {
        if #available(iOS 26.0, macOS 26.0, *) {
            self.glassEffect(.regular.interactive(), in: shape)
        } else {
            self.background(.regularMaterial).clipShape(shape)
        }
    }
}
