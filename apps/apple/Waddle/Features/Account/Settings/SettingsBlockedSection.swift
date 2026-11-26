import SwiftUI
import WaddleKit

/// The account's block list in Settings (XEP-0191): who is blocked, with
/// a way to unblock each. Blocking itself is done from a person's details.
/// The Mac Settings window is a scene of its own, so it is handed the session.
struct SettingsBlockedSection: View {
    let app: AppState

    var body: some View {
        if let session = app.session {
            BlockedPeopleSection()
                .environment(session.coordinator)
        }
    }
}

struct BlockedPeopleSection: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var failed: JID?
    @State private var working: Set<JID> = []

    var body: some View {
        let entries = session.blockedEntries
        let load = session.blockList.load
        let isOnline = session.connection == .online
        Section {
            if entries.isEmpty {
                if load == .loaded {
                    note(BlockText.empty)
                } else if !isOnline {
                    note(BlockText.offline)
                } else if load != .failed {
                    ProgressView()
                        .frame(maxWidth: .infinity)
                }
            }
            ForEach(entries) { entry in
                BlockedEntryRow(entry: entry, isWorking: working.contains(entry.jid)) {
                    unblock(entry)
                }
                .disabled(!isOnline)
            }
            // A list that could not be read does not follow changes made elsewhere, whatever it already holds.
            if load == .failed, isOnline {
                FormActionButton(title: BlockText.tryAgain, symbol: "arrow.clockwise") {
                    Task { await session.reloadBlockList() }
                }
            }
        } header: {
            FormSectionHeader(BlockText.listTitle)
        } footer: {
            if failed != nil {
                FormSectionFooter(BlockText.failed)
            } else if load == .failed, isOnline {
                FormSectionFooter(BlockText.couldNotLoad)
            } else {
                FormSectionFooter(BlockText.howToBlock)
            }
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .huddleText(Theme.Typography.body)
            .foregroundStyle(Theme.Colors.fgMuted)
    }

    private func unblock(_ entry: BlockedEntry) {
        working.insert(entry.jid)
        failed = nil
        Task {
            do {
                try await session.unblock(entry.jid)
            } catch {
                failed = entry.jid
            }
            working.remove(entry.jid)
        }
    }
}

/// Who it is, their address when the name is not it, and Unblock.
private struct BlockedEntryRow: View {
    let entry: BlockedEntry
    let isWorking: Bool
    let unblock: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(entry.title)
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                if let detail = entry.detail {
                    Text(detail)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            Spacer(minLength: Theme.Spacing.s)
            Button(BlockText.unblock, action: unblock)
                .buttonStyle(.huddle(.secondary, size: .small))
                .disabled(isWorking)
                .accessibilityLabel(Text(entry.unblockLabel))
        }
        .accessibilityElement(children: .combine)
    }
}
