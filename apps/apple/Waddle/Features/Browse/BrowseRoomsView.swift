import SwiftUI
import WaddleKit

struct BrowseRoomsView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dynamicTypeSize) private var typeSize
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var widthClass
    #endif
    @State private var model = BrowseRoomsModel()

    private var isCompact: Bool {
        #if os(iOS)
        widthClass == .compact
        #else
        false
        #endif
    }

    var body: some View {
        let page = BrowseRooms(session: session)
        ScrollView {
            VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s5) {
                header(page)
                if let failure = model.failure {
                    Label(failure, systemImage: "exclamationmark.triangle")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.dangerText)
                }
                if !page.rows.isEmpty {
                    roomList(page.rows)
                } else if page.isLoading {
                    Label {
                        Text("Finding your rooms.")
                    } icon: {
                        ProgressView().controlSize(.small)
                    }
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .frame(maxWidth: .infinity)
                } else {
                    emptyDirectory
                }
            }
            .padding(.horizontal, isCompact ? Theme.Spacing.m : WaddleTokens.Spacing.s10)
            .padding(.vertical, isCompact ? Theme.Spacing.l : WaddleTokens.Spacing.s8)
            .frame(maxWidth: 1_000, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Theme.Colors.bg)
        .environment(\.huddleGround, Theme.Colors.bg)
        .onDisappear { model.pageDidDisappear() }
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }

    private func header(_ page: BrowseRooms) -> some View {
        let layout =
            typeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: Theme.Spacing.m))
            : AnyLayout(HStackLayout(alignment: .bottom, spacing: Theme.Spacing.m))
        return layout {
            VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s1Half) {
                HStack(spacing: WaddleTokens.Spacing.s1Half) {
                    if page.liveCount > 0 {
                        Circle()
                            .fill(Theme.Colors.live)
                            .frame(width: Theme.Size.liveDot, height: Theme.Size.liveDot)
                            .accessibilityHidden(true)
                    }
                    Text(page.countText)
                        .huddleText(Theme.Typography.eyebrow)
                        .foregroundStyle(page.liveCount > 0 ? Theme.Colors.liveText : Theme.Colors.fgMuted)
                }
                Text("Browse rooms")
                    .huddleText(Theme.Typography.headline)
                    .foregroundStyle(Theme.Colors.fg)
                    .accessibilityAddTraits(.isHeader)
                Text("Sorted by what is happening: huddles first, then mentions and unread.")
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if session.isCommunityOwner && !page.rows.isEmpty {
                newRoomButton
            }
        }
    }

    private func roomList(_ rows: [BrowseRooms.Row]) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous)
        return LazyVStack(spacing: 0) {
            ForEach(rows) { row in
                RoomBrowseRow(row: row, isJoining: model.joining == row.id, showsFaces: !isCompact) {
                    Task { await model.open(row.id, using: session, navigation: navigation) }
                }
                if row.id != rows.last?.id {
                    Rectangle()
                        .fill(Theme.Colors.borderSoft)
                        .frame(height: WaddleTokens.BorderWidth.hair)
                        .accessibilityHidden(true)
                }
            }
        }
        .disabled(model.joining != nil)
        .background(Theme.Colors.surface, in: shape)
        .clipShape(shape)
        .overlay { shape.strokeBorder(Theme.Colors.borderSoft, lineWidth: WaddleTokens.BorderWidth.hair).allowsHitTesting(false) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Rooms")
    }

    private var emptyDirectory: some View {
        VStack(spacing: Theme.Spacing.s) {
            EmptyStateView(
                title: "No rooms yet",
                message: session.isCommunityOwner
                    ? "Create the first one to get people talking."
                    : "They show up here once a server owner creates one.",
                symbol: "number"
            )
            if session.isCommunityOwner { newRoomButton }
        }
        .padding(.top, WaddleTokens.Spacing.s8)
    }

    private var newRoomButton: some View {
        Button("New room") { navigation.sheet = .newChannel }
            .buttonStyle(.huddle(.primary))
            .disabled(model.joining != nil)
    }
}

/// The compass beside the Rooms heading in the split sidebar.
struct BrowseRoomsSidebarLink: View {
    @Environment(NavigationModel.self) private var navigation

    var body: some View {
        let isSelected = navigation.selection == nil && navigation.splitSection == .browseRooms
        Button {
            navigation.show(.browseRooms)
        } label: {
            NavigationIconLabel(symbol: "safari", color: isSelected ? Theme.Colors.actionText : Theme.Colors.fgSoft)
                .background(isSelected ? Theme.State.selected : .clear, in: RoundedRectangle(cornerRadius: Theme.Radius.medium))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Browse all rooms")
        .accessibilityAddTraits(isSelected ? .isSelected : [])
        .help("Browse all rooms")
    }
}
