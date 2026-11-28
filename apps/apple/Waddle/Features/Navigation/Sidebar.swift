import SwiftUI
import WaddleKit

/// iPad and Mac sidebar: the community, search, Home, Inbox and People (and
/// Admin for community owners), then rooms, group chats and direct
/// messages, with the account at the foot.
///
/// The sidebar is the dark sea in both themes, so everything in it is
/// drawn in Night colours.
struct Sidebar: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(CallWindowLayout.self) private var layout: CallWindowLayout?
    @State private var query = ""
    private let spaceExpansion: SidebarSpaceExpansion

    init(spaceExpansion: SidebarSpaceExpansion? = nil) {
        self.spaceExpansion = spaceExpansion ?? .device
    }

    var body: some View {
        let sections = currentSections
        VStack(spacing: 0) {
            SidebarHeader(name: session.directory.spaces.first?.name ?? "Waddle")
            SidebarSearchField(text: $query)
                .padding(.horizontal, Theme.Spacing.m)
                .padding(.bottom, Theme.Spacing.s)
            List {
                if !isSearching { destinations }
                SidebarRoomsSection(expansion: spaceExpansion, searchResults: isSearching ? sections.channels : nil)
                GroupChatListSection(groups: sections.groupDMs, style: .sidebar)
                DirectListSection(directs: sections.directs, style: .sidebar, isSearching: isSearching)
            }
            .navigationListStyle(ground: Theme.Colors.bgRail)
            .overlay {
                if isSearching && sections.isEmpty {
                    NoMatchesView(query: query)
                }
            }
            LiveCallsHost()
            CurrentCallCardHost()
            SidebarAccountBar()
        }
        .background(Theme.Colors.bgRail.ignoresSafeArea())
        .environment(\.huddleGround, Theme.Colors.bgRail)
        .environment(\.colorScheme, .dark)
        // The system can take the sidebar away without a word to the split view's binding.
        .onGeometryChange(for: Bool.self) { sidebar in
            let frame = sidebar.frame(in: .global)
            return frame.width > 1 && frame.maxX > 1
        } action: { isOnScreen in
            layout?.sidebarIsOnScreen = isOnScreen
        }
        .onDisappear { layout?.sidebarIsOnScreen = false }
        .modifier(RevealSidebarRoom(expansion: spaceExpansion))
        .navigationTitle("")
        #if os(iOS)
        .toolbarBackground(.hidden, for: .navigationBar)
        #endif
    }

    @ViewBuilder
    private var destinations: some View {
        SidebarDestinationRow(
            title: "Home",
            symbol: "house",
            isSelected: isShowing(.home)
        ) {
            navigation.show(.home)
        }
        SidebarDestinationRow(
            title: "Inbox",
            symbol: "tray",
            unread: session.unreadOverviewRoomCount,
            isMention: session.unread.mentions.contains { $0.isRoom },
            isSelected: isShowing(.inbox)
        ) {
            navigation.show(.inbox)
        }
        SidebarDestinationRow(
            title: "Calendar",
            symbol: "calendar",
            isSelected: isShowing(.events)
        ) {
            navigation.show(.events)
        }
        SidebarDestinationRow(
            title: "People",
            symbol: "person.2",
            isSelected: isShowing(.people)
        ) {
            navigation.show(.people)
        }
        SidebarDestinationRow(
            title: "Feed",
            symbol: "newspaper",
            isSelected: isShowing(.feed)
        ) {
            navigation.show(.feed)
        }
        if session.isCommunityOwner {
            SidebarDestinationRow(
                title: "Admin",
                symbol: "shield",
                isSelected: isShowing(.admin)
            ) {
                navigation.show(.admin)
            }
        }
    }

    private func isShowing(_ section: SplitSection) -> Bool {
        navigation.selection == nil && navigation.splitSection == section
    }

    private var isSearching: Bool {
        !ConversationNameFilter.normalized(query).isEmpty
    }

    private var currentSections: NavigationSections {
        let directory = session.directory
        return NavigationSections.build(
            channels: directory.channels,
            groupDMs: directory.groupDMs,
            directs: directory.directConversations,
            query: query
        )
    }
}

/// The community's mark and name, and the button that starts a new message.
private struct SidebarHeader: View {
    @Environment(NavigationModel.self) private var navigation
    let name: String

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            WaddleBrandMark(size: 28)
            Text(name)
                .huddleText(Theme.Typography.channel)
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(1)
            Spacer(minLength: 0)
            Button {
                navigation.sheet = .newMessage
            } label: {
                NavigationIconLabel(symbol: "square.and.pencil")
            }
            .buttonStyle(.plain)
            .help("New message")
            .accessibilityLabel(Text("New message"))
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.top, Theme.Spacing.s)
        .padding(.bottom, Theme.Spacing.m)
    }
}

/// Filters the sidebar by name as you type.
private struct SidebarSearchField: View {
    @Binding var text: String

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Theme.Colors.fgSoft)
                .accessibilityHidden(true)
            TextField("Search rooms and people", text: $text)
                .textFieldStyle(.plain)
                .huddleText(Theme.Typography.input)
                .foregroundStyle(Theme.Colors.fg)
            if !text.isEmpty {
                Button {
                    text = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(Theme.Colors.fgSoft)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text("Clear search"))
            }
        }
        .padding(.horizontal, Theme.Spacing.s + Theme.Spacing.xxs)
        .frame(minHeight: WaddleTokens.ControlHeight.md)
        .background(
            RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous)
                .fill(Theme.Colors.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous)
                .strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
        )
    }
}

/// Home or Inbox: a place in the app that is not a conversation.
private struct SidebarDestinationRow: View {
    let title: String
    let symbol: String
    var unread = 0
    var isMention = false
    let isSelected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            NavigationRowChrome(isSelected: isSelected) {
                NavigationRowLabel(title: title, isEmphasised: isSelected || unread > 0) {
                    Image(systemName: symbol)
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(isSelected ? Theme.Colors.fg : Theme.Colors.fgMuted)
                } trailing: {
                    UnreadBadge(count: unread, isMention: isMention)
                }
            }
        }
        .buttonStyle(.plain)
        .navigationListRow()
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}
