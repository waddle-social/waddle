import SwiftUI

struct SettingsMessagesSection: View {
    @Bindable var preferences: Preferences

    var body: some View {
        @Bindable var messageSounds = preferences.messageSounds
        Section {
            VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                SettingsMessageOrderPicker(selection: $preferences.messageOrder)
                FormSectionFooter(orderDescription)
            }
            FormToggle(
                "Message sounds",
                caption: messageSounds.isEnabled
                    ? "In-app sounds are on for this device."
                    : "In-app sounds are off for this device.",
                isOn: $messageSounds.isEnabled
            )
        } header: {
            FormSectionHeader("Messages")
        }
    }

    private var orderDescription: String {
        switch preferences.messageOrder {
        case .chat:
            return "The classic chat flow: new messages arrive at the bottom, next to the composer."
        case .social:
            return "Read like a feed: the composer sits at the top and the newest messages come first as you scroll down."
        }
    }
}
