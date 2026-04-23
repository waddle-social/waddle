import SwiftUI

struct WaddleBrandMark: View {
    var size: CGFloat = 44

    var body: some View {
        Image("WaddleLogo")
            .resizable()
            .interpolation(.high)
            .antialiased(true)
            .scaledToFit()
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}
