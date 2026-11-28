import SwiftUI
import WaddleKit

/// The name and description of a space or a room.
struct AdminNameFields: View {
    @Binding var name: String
    @Binding var description: String
    let namePrompt: String
    let descriptionPrompt: String
    /// It has a description and the field is blank: the server reads a
    /// blank one as "leave it".
    let keepsOldDescription: Bool
    let submit: () -> Void
    var descriptionTitle = "Description"

    var body: some View {
        Section {
            TextField("Name", text: $name, prompt: Text(namePrompt))
                .labelsHidden()
                .huddleField()
                .onSubmit(submit)
                .listRowBackground(Theme.Colors.bg)
        } header: {
            FormSectionHeader("Name")
        } footer: {
            if AdminName.isTooLong(name) {
                Text("A name can be up to \(AdminName.maxLength) characters.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        Section {
            TextField("Description", text: $description, prompt: Text(descriptionPrompt), axis: .vertical)
                .labelsHidden()
                .lineLimit(2...5)
                .huddleField()
                .listRowBackground(Theme.Colors.bg)
        } header: {
            FormSectionHeader(descriptionTitle)
        } footer: {
            if keepsOldDescription {
                FormSectionFooter("A description can be changed, but not removed yet.")
            }
        }
    }
}
