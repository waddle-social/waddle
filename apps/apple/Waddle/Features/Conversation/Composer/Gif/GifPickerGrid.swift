import SwiftUI
import WaddleKit

/// Search hits in an adaptive grid.
struct GifPickerGrid: View {
    let items: [GifSearchItem]
    let onPick: (GifSearchItem) -> Void

    private let columns = [GridItem(.adaptive(minimum: 120), spacing: Theme.Spacing.xs)]

    var body: some View {
        ScrollView {
            LazyVGrid(columns: columns, spacing: Theme.Spacing.xs) {
                ForEach(items) { item in
                    GifPickerCell(item: item) {
                        onPick(item)
                    }
                }
            }
            .padding(Theme.Spacing.s)
        }
    }
}
