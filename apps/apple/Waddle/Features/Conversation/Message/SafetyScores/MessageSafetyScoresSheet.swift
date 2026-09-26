import SwiftUI
import WaddleKit

/// Per-category breakdown of the scores the room fastened to a message:
/// only the categories at or above the notice threshold, so the reader
/// sees what raised the marker and nothing else. Reads the live row, so a
/// re-judgment or a clear shows while it is open.
struct MessageSafetyScoresSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    let item: TimelineItem

    var body: some View {
        NavigationStack {
            content
                .navigationTitle(MessageSafetyScoresButton.title)
                #if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
                #endif
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { dismiss() }
                    }
                }
        }
        .presentationDetents([.medium, .large])
        #if os(macOS)
        .frame(minWidth: 360, minHeight: 380)
        #endif
    }

    /// The loaded row's scores; the presented snapshot only when the row
    /// has left the timeline.
    private var scores: SafetyScores? {
        let timeline = session.timelines.timeline(for: item.conversation)
        guard let current = timeline.item(withID: item.id) else { return item.safetyScores }
        return current.safetyScores
    }

    @ViewBuilder
    private var content: some View {
        if let scores, !scores.notableRows.isEmpty {
            List {
                if !scores.notableSignalRows.isEmpty {
                    Section("Community") {
                        ForEach(scores.notableSignalRows) { MessageSafetyScoreRowView(row: $0) }
                    }
                }
                if !scores.notableSafetyRows.isEmpty {
                    Section("Safety") {
                        ForEach(scores.notableSafetyRows) { MessageSafetyScoreRowView(row: $0) }
                    }
                }
                Section {
                    Text(explanation(modelVersion: scores.modelVersion))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
        } else {
            ContentUnavailableView(
                "No signals",
                systemImage: MessageSafetyScoresButton.symbol,
                description: Text("No category on this message reached the notice threshold.")
            )
        }
    }

    private func explanation(modelVersion: String) -> String {
        "Automated estimates of how likely each category applies, from \(modelVersion). Only categories at 50% or more are listed. Everyone in this conversation can see them. They are not a moderation decision."
    }
}

/// One category: title, percentage and a bar.
struct MessageSafetyScoreRowView: View {
    let row: SafetyScoreRow

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(alignment: .firstTextBaseline) {
                Text(row.title)
                Spacer(minLength: Theme.Spacing.s)
                Text(row.percentText)
                    .font(.body.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            ProgressView(value: row.probability)
                .tint(row.severity.map(MessageSafetyScoresButton.tint(for:)) ?? Color.secondary)
                .accessibilityHidden(true)
        }
        .padding(.vertical, Theme.Spacing.xxs)
        .help(row.taxonomyVersion)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(row.title))
        .accessibilityValue(Text(row.percentText))
    }
}
