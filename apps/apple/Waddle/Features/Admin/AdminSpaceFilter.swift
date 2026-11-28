import SwiftUI
import WaddleKit

/// Narrows the Rooms list to one of the spaces the console has loaded.
struct AdminSpaceFilter: View {
    let spaces: [AdminSpace]
    @Binding var selection: AdminSpace?

    var body: some View {
        HuddlePicker(title: "Space", valueTitle: selection?.name ?? "All spaces", selection: $selection) {
            Text("All spaces").tag(AdminSpace?.none)
            ForEach(spaces) { space in
                Text(space.name).tag(AdminSpace?.some(space))
            }
        }
    }
}
