import SwiftUI
import WaddleKit

/// Search hits for the recipient field, plus the typed address itself.
struct RecipientResultsSection: View {
    let typedAddress: BareJID?
    let search: UserSearchState
    let tokens: [RecipientToken]
    let onToggle: (RecipientToken) -> Void

    var body: some View {
        Section {
            if let typedAddress, !search.results.contains(where: { $0.jid == typedAddress }) {
                RecipientResultRow(
                    token: RecipientToken(jid: typedAddress, name: nil),
                    detail: typedAddress.description,
                    isPicked: isPicked(typedAddress),
                    onToggle: onToggle
                )
            }
            ForEach(search.results) { result in
                RecipientResultRow(
                    token: RecipientToken(jid: result.jid, name: result.displayName),
                    detail: result.jid.description,
                    isPicked: isPicked(result.jid),
                    onToggle: onToggle
                )
            }
            statusRow
        } header: {
            Text("People")
        }
    }

    @ViewBuilder
    private var statusRow: some View {
        if search.isSearching {
            NavigationLoadingRow(title: "Searching…")
        } else if let message = search.errorMessage {
            Label(message, systemImage: "exclamationmark.triangle")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        } else if search.hasSearched && search.results.isEmpty && typedAddress == nil {
            Text("No people found. You can also type a full address.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
    }

    private func isPicked(_ jid: BareJID) -> Bool {
        tokens.contains { $0.jid == jid }
    }
}

/// A person that can be picked or unpicked.
struct RecipientResultRow: View {
    let token: RecipientToken
    let detail: String
    let isPicked: Bool
    let onToggle: (RecipientToken) -> Void

    var body: some View {
        Button {
            onToggle(token)
        } label: {
            HStack(spacing: Theme.Spacing.m) {
                JIDAvatar(jid: token.jid, name: token.name, size: 32)
                VStack(alignment: .leading, spacing: 0) {
                    Text(token.name)
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: Theme.Spacing.s)
                Image(systemName: isPicked ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(isPicked ? Color.accentColor : Color.secondary)
                    .imageScale(.large)
            }
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(token.name), \(detail)"))
        .accessibilityAddTraits(traits)
    }

    private var traits: AccessibilityTraits {
        isPicked ? [.isButton, .isSelected] : [.isButton]
    }
}
