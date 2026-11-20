import SwiftUI
import WaddleKit

/// The whole pack a sticker in a conversation is from, with a way to add
/// it to your own stickers.
struct StickerPackSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    let location: StickerPackLocation
    /// Snapshots draw this instead of fetching each sticker's picture.
    var samplePicture: Image?

    @State private var phase: Phase = .loading
    @State private var isAdding = false
    /// Added from this sheet. The copy can end up under another id than
    /// the pack shown, so the list of own packs alone does not tell.
    @State private var wasAdded = false
    @State private var errorMessage: String?

    private enum Phase {
        case loading
        case loaded(StickerPack)
        /// The pack is no longer where the sticker says.
        case gone
        case failed(String)
    }

    private let columns = [GridItem(.adaptive(minimum: StickerTile.side), spacing: Theme.Spacing.xs)]

    var body: some View {
        NavigationStack {
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Theme.Colors.bg)
                .navigationTitle("Sticker pack")
                #if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
                #endif
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Close", systemImage: "xmark") { dismiss() }
                            .help("Close")
                    }
                }
        }
        .presentationDetents([.medium, .large])
        #if os(macOS)
        .frame(minWidth: 440, minHeight: 480)
        #endif
        .task { await load() }
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .loading:
            ProgressView()
        case let .loaded(pack):
            page(pack)
        case .gone:
            EmptyStateView(
                title: "This pack is gone",
                message: "The person who made it has taken it down.",
                symbol: "face.dashed"
            )
        case let .failed(message):
            VStack(spacing: 0) {
                EmptyStateView(title: "Couldn't open this pack", message: message, symbol: "exclamationmark.triangle")
                    .fixedSize(horizontal: false, vertical: true)
                Button("Try again") { Task { await load() } }
                    .buttonStyle(.huddle(.primary))
            }
        }
    }

    private func page(_ pack: StickerPack) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text(pack.title)
                        .huddleText(Theme.Typography.label)
                        .foregroundStyle(Theme.Colors.fg)
                        .accessibilityAddTraits(.isHeader)
                    if let summary = pack.summary, !summary.isEmpty {
                        Text(summary)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgMuted)
                    }
                }
                LazyVGrid(columns: columns, alignment: .leading, spacing: Theme.Spacing.xs) {
                    ForEach(Array(pack.stickers.enumerated()), id: \.offset) { _, sticker in
                        StickerTile(sticker: sticker, samplePicture: samplePicture)
                            .accessibilityLabel(Text(sticker.text))
                            .accessibilityAddTraits(.isImage)
                    }
                }
                ownership(of: pack)
                if let errorMessage {
                    Label(errorMessage, systemImage: "exclamationmark.triangle")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.dangerText)
                }
            }
            .padding(Theme.Spacing.l)
        }
    }

    /// Add the pack, or say why there is nothing to add.
    @ViewBuilder
    private func ownership(of pack: StickerPack) -> some View {
        if wasAdded || location.owner == session.account.jid || session.stickers.packs.contains(where: { $0.id == pack.id }) {
            Label("In your stickers", systemImage: "checkmark.circle")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
        } else if pack.isRestricted {
            Text("The person who made this pack keeps it to themselves, so it can't be added to yours.")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
        } else {
            Button(isAdding ? "Adding…" : "Add to my stickers") { add(pack) }
                .buttonStyle(.huddle(.primary))
                .disabled(isAdding)
        }
    }

    private func load() async {
        phase = .loading
        // Which packs are already yours decides what the page offers.
        if session.stickers.phase == .idle {
            await session.loadStickerPacks()
        }
        do {
            phase = try await session.stickerPack(at: location).map(Phase.loaded) ?? .gone
        } catch {
            guard !Task.isCancelled else { return }
            phase = .failed(ActionErrorCopy.message(for: error, fallback: "Check your connection and try again."))
        }
    }

    private func add(_ pack: StickerPack) {
        isAdding = true
        errorMessage = nil
        Task {
            defer { isAdding = false }
            do {
                try await session.copyStickerPack(pack, from: location)
                wasAdded = true
            } catch PortError.invalidRequest {
                // The pack itself is one that cannot be published, for
                // example a sticker in it has no description: nothing to
                // check and try again.
                errorMessage = "\(pack.title) can't be added to your stickers."
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't add \(pack.title). Try again.")
            }
        }
    }
}
