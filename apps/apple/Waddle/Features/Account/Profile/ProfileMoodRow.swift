import SwiftUI
import WaddleKit

/// Current XEP-0107 mood, linking to the mood editor.
struct ProfileMoodRow: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        NavigationLink {
            ProfileMoodEditor(current: session.status.mood)
        } label: {
            HStack(spacing: Theme.Spacing.m) {
                ProfileMoodGlyph(mood: session.status.mood)
                ProfileMoodSummary(mood: session.status.mood)
            }
        }
    }
}

/// The mood's emoji, or a neutral symbol when none is set.
private struct ProfileMoodGlyph: View {
    let mood: UserMood?

    var body: some View {
        Group {
            if let mood {
                Text(ProfileMoodCatalog.emoji(for: mood.value))
            } else {
                Image(systemName: "face.smiling")
                    .foregroundStyle(.secondary)
            }
        }
        .font(.title2)
        .frame(width: 32)
        .accessibilityHidden(true)
    }
}

/// Mood name and note, or a prompt when none is set.
private struct ProfileMoodSummary: View {
    let mood: UserMood?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            if let mood {
                Text(ProfileMoodCatalog.title(for: mood.value))
                if let text = mood.text {
                    Text(text)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            } else {
                Text("Set a mood")
                Text("Let people know how you're feeling.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
    }
}
