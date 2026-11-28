import SwiftUI
import WaddleKit

/// Room type and access, with a space choice while creating a room.
struct AdminRoomSettings: View {
    @Binding var kind: AdminRoomKind
    @Binding var access: AdminRoomAccess
    let spaceSelection: Binding<AdminSpace.ID?>?
    let spaces: AdminList<AdminSpace>?
    let spacesAreSettled: Bool

    var body: some View {
        Section {
            kindRow
                .listRowBackground(Theme.Colors.surface)
            if let selection = spaceSelection {
                spaceRows(selection)
            }
        }
        Section {
            FormToggle("People can find it", isOn: $access.isListed)
                .onChange(of: access.isListed) { _, isListed in
                    // Hiding a room suggests keeping it to its members;
                    // the next switch can say otherwise.
                    if !isListed { access.isMembersOnly = true }
                }
                .listRowBackground(Theme.Colors.surface)
            FormToggle("Only members can join", isOn: $access.isMembersOnly)
                .listRowBackground(Theme.Colors.surface)
        } footer: {
            if !access.isListed, !access.isMembersOnly {
                FormSectionFooter("Anyone who has the room's address can join it.")
            }
        }
    }

    @ViewBuilder
    private var kindRow: some View {
        if kind.canBeChosen {
            selectionRow("Type") {
                HuddlePicker(title: "Kind", valueTitle: AdminRoomCopy.title(kind), selection: $kind) {
                    ForEach(AdminRoomKind.choices, id: \.self) { choice in
                        Text(AdminRoomCopy.title(choice)).tag(choice)
                    }
                }
            }
        } else {
            FormValueRow(title: "Type", value: AdminRoomCopy.title(kind))
                .accessibilityLabel(Text("Kind"))
        }
    }

    @ViewBuilder
    private func spaceRows(_ selection: Binding<AdminSpace.ID?>) -> some View {
        if let spaces, spacesAreSettled {
            if !spaces.rows.isEmpty {
                let name = spaces.rows.first { $0.id == selection.wrappedValue }?.name ?? "No space"
                selectionRow("Space (optional)") {
                    HuddlePicker(title: "Space", valueTitle: name, selection: selection) {
                        Text("No space").tag(AdminSpace.ID?.none)
                        ForEach(spaces.rows) { space in
                            Text(space.name).tag(AdminSpace.ID?.some(space.id))
                        }
                    }
                }
                .listRowBackground(Theme.Colors.surface)
            }
            if spaces.failure != nil || spaces.moreFailure != nil {
                selectionRow(spaces.rows.isEmpty ? "Spaces didn't load" : "Some spaces didn't load") {
                    Button("Try again") { Task { await spaces.loadEveryPage() } }
                        .buttonStyle(.huddle(size: .small))
                }
                .listRowBackground(Theme.Colors.surface)
            }
        } else {
            FormValueRow(title: "Space", value: "Loading…")
                .listRowBackground(Theme.Colors.surface)
        }
    }

    private func selectionRow<Control: View>(_ title: String, @ViewBuilder control: () -> Control) -> some View {
        LabeledContent {
            control()
        } label: {
            Text(title)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fg)
        }
        .frame(minHeight: Theme.Size.formRow)
    }
}
