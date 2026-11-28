import SwiftUI
import WaddleKit

struct EventsFilterChip: View {
    @Binding var filter: EventFilter

    var body: some View {
        let isSelected = filter == .goingOrMaybe
        Button("Going or maybe") {
            filter = isSelected ? .all : .goingOrMaybe
        }
        .buttonStyle(.huddleChip(isSelected: isSelected))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}
