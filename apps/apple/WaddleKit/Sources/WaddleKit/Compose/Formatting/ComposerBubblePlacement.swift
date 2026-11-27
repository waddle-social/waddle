import Foundation

public enum ComposerBubblePlacement {
    /// Places the bubble in screen coordinates, where positive y is up.
    public static func frame(
        for size: CGSize, selection: CGRect, windowContent: CGRect, screenVisible: CGRect, gap: CGFloat
    ) -> CGRect? {
        let available = windowContent.intersection(screenVisible)
        guard !available.isEmpty, size.width > 0, size.height > 0,
              size.width <= available.width, size.height <= available.height
        else { return nil }

        let above = selection.maxY + gap
        let y = above + size.height <= available.maxY ? above : selection.minY - gap - size.height
        let origin = CGPoint(
            x: min(max(selection.midX - size.width / 2, available.minX), available.maxX - size.width),
            y: min(max(y, available.minY), available.maxY - size.height)
        )
        return CGRect(origin: origin, size: size)
    }
}
