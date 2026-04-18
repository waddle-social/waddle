#if os(iOS)
import SwiftUI

private enum MobileWorkspaceFilter: String, CaseIterable, Identifiable {
    case joined
    case discover
    case all

    var id: String { rawValue }

    var title: String {
        switch self {
        case .joined:
            return "Joined"
        case .discover:
            return "Discover"
        case .all:
            return "All"
        }
    }
}

struct MobileSlackShellView: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    @AppStorage(AppConfig.mobileShellTabKey) private var selectedTabRaw = MobileShellTab.home.rawValue
    @AppStorage(AppConfig.mobileWorkspaceFilterKey) private var workspaceFilterRaw = MobileWorkspaceFilter.joined.rawValue
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private enum MobileShellTab: String, Hashable {
        case home
        case browse
        case chat
        case you
    }

    private var selectedTab: Binding<MobileShellTab> {
        Binding(
            get: { MobileShellTab(rawValue: selectedTabRaw) ?? .home },
            set: { selectedTabRaw = $0.rawValue }
        )
    }

    private var workspaceFilter: Binding<MobileWorkspaceFilter> {
        Binding(
            get: { MobileWorkspaceFilter(rawValue: workspaceFilterRaw) ?? .joined },
            set: { workspaceFilterRaw = $0.rawValue }
        )
    }

    private var joinedWaddles: [WaddleSummary] {
        model.publicWaddles.filter { model.isJoined($0.id) }
    }

    private var discoveredWaddles: [WaddleSummary] {
        model.publicWaddles.filter { !model.isJoined($0.id) }
    }

    private var serverLabel: String {
        if let url = AppConfig.normalizedServerURL(from: model.serverURLText) {
            return url.host ?? url.absoluteString
        }

        return model.serverURLText
    }

    var body: some View {
        TabView(selection: selectedTab) {
            NavigationStack {
                MobileHomeTab(
                    session: model.session,
                    serverLabel: serverLabel,
                    selectedWaddle: model.selectedWaddle,
                    selectedChannelID: model.selectedChannelID,
                    channels: model.channels,
                    joinedWaddles: joinedWaddles,
                    discoveredWaddles: discoveredWaddles,
                    isLoadingStructure: model.isLoadingStructure,
                    onCreate: { showCreateSheet = true },
                    onBrowse: { updateSelectedTab(.browse) },
                    onOpenCurrentChat: { updateSelectedTab(.chat) },
                    onOpenWaddle: openWaddle(_:),
                    onJoinWaddle: joinWaddle(_:)
                )
            }
            .tabItem {
                Label("Home", systemImage: "house.fill")
            }
            .tag(MobileShellTab.home)

            NavigationStack {
                MobileWorkspaceBrowserTab(
                    model: model,
                    serverLabel: serverLabel,
                    filter: workspaceFilter,
                    joinedWaddles: joinedWaddles,
                    discoveredWaddles: discoveredWaddles,
                    onCreate: { showCreateSheet = true },
                    onOpenWaddle: openWaddle(_:),
                    onJoinWaddle: joinWaddle(_:)
                )
            }
            .tabItem {
                Label("Browse", systemImage: "square.grid.2x2.fill")
            }
            .tag(MobileShellTab.browse)

            NavigationStack {
                MobileConversationTab(model: model)
                    .toolbar(.hidden, for: .navigationBar)
            }
            .tabItem {
                Label("Chat", systemImage: "bubble.left.and.bubble.right.fill")
            }
            .tag(MobileShellTab.chat)

            NavigationStack {
                MobileYouTab(
                    model: model,
                    serverLabel: serverLabel
                )
            }
            .tabItem {
                Label("You", systemImage: "person.crop.circle.fill")
            }
            .tag(MobileShellTab.you)
        }
        .tint(.accentColor)
    }

    private func updateSelectedTab(_ tab: MobileShellTab) {
        if reduceMotion {
            selectedTabRaw = tab.rawValue
        } else {
            withAnimation(.snappy(duration: 0.28, extraBounce: 0.02)) {
                selectedTabRaw = tab.rawValue
            }
        }
    }

    private func openWaddle(_ waddle: WaddleSummary) {
        Task {
            await model.selectWaddle(waddle.id)
            await MainActor.run {
                updateSelectedTab(.chat)
            }
        }
    }

    private func joinWaddle(_ waddle: WaddleSummary) {
        Task {
            await model.join(waddle)
            await MainActor.run {
                updateSelectedTab(.chat)
            }
        }
    }
}

private struct MobileHomeTab: View {
    let session: WaddleSession?
    let serverLabel: String
    let selectedWaddle: WaddleSummary?
    let selectedChannelID: String?
    let channels: [ChannelSummary]
    let joinedWaddles: [WaddleSummary]
    let discoveredWaddles: [WaddleSummary]
    let isLoadingStructure: Bool
    let onCreate: () -> Void
    let onBrowse: () -> Void
    let onOpenCurrentChat: () -> Void
    let onOpenWaddle: (WaddleSummary) -> Void
    let onJoinWaddle: (WaddleSummary) -> Void

    private var switcherWaddles: [WaddleSummary] {
        joinedWaddles.filter { $0.id != selectedWaddle?.id }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                MobileHeroCard(
                    session: session,
                    serverLabel: serverLabel,
                    joinedCount: joinedWaddles.count,
                    activeChannelCount: channels.count
                )

                currentWorkspaceCard

                if !switcherWaddles.isEmpty {
                    sectionHeader("Switch fast")

                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 12) {
                            ForEach(switcherWaddles.prefix(8)) { waddle in
                                MobileWorkspaceMiniCard(waddle: waddle) {
                                    onOpenWaddle(waddle)
                                }
                            }
                        }
                        .padding(.horizontal, 20)
                        .padding(.bottom, 2)
                    }
                }

                if !discoveredWaddles.isEmpty {
                    sectionHeader("Suggested spaces")

                    VStack(spacing: 12) {
                        ForEach(discoveredWaddles.prefix(3)) { waddle in
                            MobileWorkspaceCard(
                                waddle: waddle,
                                isJoined: false,
                                isCurrent: false,
                                primaryActionTitle: "Explore",
                                onPrimaryAction: {
                                    onOpenWaddle(waddle)
                                },
                                onSecondaryAction: {
                                    onJoinWaddle(waddle)
                                }
                            )
                        }
                    }
                    .padding(.horizontal, 20)
                }
            }
            .padding(.top, 16)
            .padding(.bottom, 28)
        }
        .background(MobileShellBackground())
        .navigationTitle("Home")
        .navigationBarTitleDisplayMode(.large)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    onCreate()
                } label: {
                    Image(systemName: "plus")
                }
            }
        }
    }

    @ViewBuilder
    private var currentWorkspaceCard: some View {
        sectionHeader("Current workspace")

        Group {
            if let selectedWaddle {
                VStack(alignment: .leading, spacing: 16) {
                    HStack(alignment: .top, spacing: 14) {
                        MobileWorkspaceAvatar(name: selectedWaddle.name, size: 48)

                        VStack(alignment: .leading, spacing: 6) {
                            Text(selectedWaddle.name)
                                .font(.system(.title3, design: .rounded, weight: .semibold))

                            if let description = selectedWaddle.description, !description.isEmpty {
                                Text(description)
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(3)
                            } else {
                                Text("Purpose-built for quick team chat on iPhone.")
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                            }
                        }

                        Spacer(minLength: 12)

                        if isLoadingStructure {
                            ProgressView()
                        } else {
                            Text("Live")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 6)
                                .background(.quaternary.opacity(0.8), in: Capsule())
                        }
                    }

                    if channels.isEmpty {
                        Text("Pick this space to load channels and start chatting.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    } else {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 8) {
                                ForEach(channels.prefix(8)) { channel in
                                    MobileChannelPill(
                                        title: channel.name,
                                        subtitle: channel.channelType,
                                        isSelected: channel.id == selectedChannelID
                                    )
                                }
                            }
                        }
                    }

                    HStack(spacing: 10) {
                        Button("Open chat") {
                            onOpenCurrentChat()
                        }
                        .buttonStyle(.borderedProminent)

                        Button("Browse spaces") {
                            onBrowse()
                        }
                        .buttonStyle(.bordered)
                    }
                }
                .mobileShellCard()
                .padding(.horizontal, 20)
            } else {
                VStack(alignment: .leading, spacing: 14) {
                    Text("Choose a space to make this shell yours.")
                        .font(.system(.title3, design: .rounded, weight: .semibold))

                    Text("Browse joined spaces, explore public rooms, and jump into the current chat with a single tap.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)

                    HStack(spacing: 10) {
                        Button("Browse spaces") {
                            onBrowse()
                        }
                        .buttonStyle(.borderedProminent)

                        Button("Create waddle") {
                            onCreate()
                        }
                        .buttonStyle(.bordered)
                    }
                }
                .mobileShellCard()
                .padding(.horizontal, 20)
            }
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(title.uppercased())
            .font(.caption.weight(.bold))
            .foregroundStyle(.secondary)
            .tracking(0.8)
            .padding(.horizontal, 20)
    }
}

private struct MobileWorkspaceBrowserTab: View {
    @ObservedObject var model: AppModel
    let serverLabel: String
    @Binding var filter: MobileWorkspaceFilter
    let joinedWaddles: [WaddleSummary]
    let discoveredWaddles: [WaddleSummary]
    let onCreate: () -> Void
    let onOpenWaddle: (WaddleSummary) -> Void
    let onJoinWaddle: (WaddleSummary) -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                MobileHeaderStrip(
                    title: "Workspace browser",
                    subtitle: serverLabel
                )
                .padding(.horizontal, 20)

                Picker("Filter", selection: $filter) {
                    ForEach(MobileWorkspaceFilter.allCases) { filter in
                        Text(filter.title).tag(filter)
                    }
                }
                .pickerStyle(.segmented)
                .padding(.horizontal, 20)

                switch filter {
                case .joined:
                    workspaceSection(
                        title: "Joined spaces",
                        caption: "Keep the main places you talk in one reachable list.",
                        waddles: joinedWaddles
                    )
                case .discover:
                    workspaceSection(
                        title: "Discover public spaces",
                        caption: "Explore more channels without crowding your home tab.",
                        waddles: discoveredWaddles
                    )
                case .all:
                    workspaceSection(
                        title: "Joined spaces",
                        caption: "Your active team spaces.",
                        waddles: joinedWaddles
                    )
                    workspaceSection(
                        title: "Discover public spaces",
                        caption: "Public rooms ready to explore.",
                        waddles: discoveredWaddles
                    )
                }
            }
            .padding(.top, 16)
            .padding(.bottom, 28)
        }
        .background(MobileShellBackground())
        .navigationTitle("Browse")
        .navigationBarTitleDisplayMode(.large)
        .searchable(text: $model.searchQuery, prompt: "Search workspaces")
        .onChange(of: model.searchQuery) { _, _ in
            model.schedulePublicWaddleSearch()
        }
        .refreshable {
            await model.refreshPublicWaddles()
        }
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                if model.isLoadingWaddles {
                    ProgressView()
                }

                Button {
                    Task { await model.refreshPublicWaddles() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }

                Button {
                    onCreate()
                } label: {
                    Image(systemName: "plus")
                }
            }
        }
        .task {
            if model.publicWaddles.isEmpty {
                await model.refreshPublicWaddles()
            }
        }
    }

    @ViewBuilder
    private func workspaceSection(title: String, caption: String, waddles: [WaddleSummary]) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.headline)
                Text(caption)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 20)

            if waddles.isEmpty {
                MobileEmptyCard(
                    title: "Nothing here yet",
                    message: "Try a different search or refresh to load more spaces."
                )
                .padding(.horizontal, 20)
            } else {
                VStack(spacing: 12) {
                    ForEach(waddles) { waddle in
                        MobileWorkspaceCard(
                            waddle: waddle,
                            isJoined: model.isJoined(waddle.id),
                            isCurrent: model.selectedWaddleID == waddle.id,
                            primaryActionTitle: model.isJoined(waddle.id) ? "Open chat" : "Explore",
                            onPrimaryAction: {
                                onOpenWaddle(waddle)
                            },
                            onSecondaryAction: model.isJoined(waddle.id) ? nil : {
                                onJoinWaddle(waddle)
                            }
                        )
                    }
                }
                .padding(.horizontal, 20)
            }
        }
    }
}

private struct MobileConversationTab: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Group {
            if let waddle = model.selectedWaddle {
                WaddleChatWorkspaceView(model: model, store: model.chatStore, waddle: waddle)
            } else {
                MobileEmptyCard(
                    title: "Pick a space first",
                    message: "Use Browse to choose a workspace, then your live chat opens here."
                )
                .padding(.horizontal, 20)
                .padding(.top, 24)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                .background(MobileShellBackground())
            }
        }
        .background(MobileShellBackground())
    }
}

private struct MobileYouTab: View {
    @ObservedObject var model: AppModel
    let serverLabel: String
    @AppStorage(AppConfig.themePreferenceKey) private var themePreferenceRaw = AppThemePreference.system.rawValue
    @AppStorage(AppConfig.scrollDirectionKey) private var scrollDirectionRaw = ChatScrollDirection.chat.rawValue

    private var themePreference: AppThemePreference {
        AppThemePreference(rawValue: themePreferenceRaw) ?? .system
    }

    private var scrollDirection: ChatScrollDirection {
        ChatScrollDirection(rawValue: scrollDirectionRaw) ?? .chat
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 14) {
                    HStack(spacing: 14) {
                        MobileWorkspaceAvatar(name: model.session?.username ?? "You", size: 54)

                        VStack(alignment: .leading, spacing: 4) {
                            Text(model.session?.username ?? "Signed in")
                                .font(.system(.title3, design: .rounded, weight: .semibold))
                            Text(serverLabel)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }

                        Spacer()
                    }

                    HStack(spacing: 10) {
                        Label("\(model.joinedWaddleIDs.count)", systemImage: "person.3.fill")
                            .mobileStatPill()
                        Label("\(model.channels.count)", systemImage: "number")
                            .mobileStatPill()
                    }
                }
                .mobileShellCard()
                .padding(.horizontal, 20)

                VStack(alignment: .leading, spacing: 14) {
                    Text("Appearance")
                        .font(.headline)

                    Picker("Theme", selection: $themePreferenceRaw) {
                        ForEach(AppThemePreference.allCases) { preference in
                            Text(preference.title).tag(preference.rawValue)
                        }
                    }
                    .pickerStyle(.segmented)

                    Text("Using \(themePreference.title.lowercased()) theme.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                .mobileShellCard()
                .padding(.horizontal, 20)

                VStack(alignment: .leading, spacing: 14) {
                    Text("Conversation layout")
                        .font(.headline)

                    Picker("Scroll direction", selection: $scrollDirectionRaw) {
                        ForEach(ChatScrollDirection.allCases) { direction in
                            Text(direction.title).tag(direction.rawValue)
                        }
                    }
                    .pickerStyle(.segmented)

                    Text(scrollDirection.description)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                .mobileShellCard()
                .padding(.horizontal, 20)

                VStack(alignment: .leading, spacing: 14) {
                    Text("Session")
                        .font(.headline)

                    Button("Sign out", role: .destructive) {
                        Task { await model.signOut() }
                    }
                    .buttonStyle(.bordered)
                }
                .mobileShellCard()
                .padding(.horizontal, 20)
            }
            .padding(.top, 16)
            .padding(.bottom, 28)
        }
        .background(MobileShellBackground())
        .navigationTitle("You")
        .navigationBarTitleDisplayMode(.large)
    }
}

private struct MobileHeroCard: View {
    let session: WaddleSession?
    let serverLabel: String
    let joinedCount: Int
    let activeChannelCount: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .top, spacing: 14) {
                MobileWorkspaceAvatar(name: session?.username ?? "Waddle", size: 52)

                VStack(alignment: .leading, spacing: 6) {
                    Text("Waddle")
                        .font(.system(size: 30, weight: .bold, design: .rounded))

                    Text("A calmer, sharper phone shell for switching teams and dropping into chat.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }

                Spacer(minLength: 12)
            }

            HStack(spacing: 10) {
                Label(serverLabel, systemImage: "network")
                    .mobileStatPill()
                Label("\(joinedCount) spaces", systemImage: "square.stack.3d.up.fill")
                    .mobileStatPill()
                Label("\(activeChannelCount) channels", systemImage: "bubble.left.and.bubble.right")
                    .mobileStatPill()
            }
        }
        .mobileShellCard(padding: 20)
        .padding(.horizontal, 20)
    }
}

private struct MobileHeaderStrip: View {
    let title: String
    let subtitle: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.system(.title3, design: .rounded, weight: .semibold))
            Text(subtitle)
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
        .mobileShellCard()
    }
}

private struct MobileWorkspaceCard: View {
    let waddle: WaddleSummary
    let isJoined: Bool
    let isCurrent: Bool
    let primaryActionTitle: String
    let onPrimaryAction: () -> Void
    let onSecondaryAction: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 12) {
                MobileWorkspaceAvatar(name: waddle.name, size: 42)

                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 8) {
                        Text(waddle.name)
                            .font(.headline)

                        if isCurrent {
                            Text("Current")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 4)
                                .background(.quaternary.opacity(0.9), in: Capsule())
                        }
                    }

                    if let description = waddle.description, !description.isEmpty {
                        Text(description)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                    } else {
                        Text(isJoined ? "Ready for quick switching on mobile." : "Public space available to explore.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                }

                Spacer(minLength: 12)
            }

            HStack(spacing: 8) {
                Label(isJoined ? "Joined" : "Public", systemImage: isJoined ? "checkmark.circle.fill" : "globe")
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(isJoined ? .green : .secondary)

                if let role = waddle.role, !role.isEmpty {
                    Label(role.capitalized, systemImage: "person.crop.circle")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }

            HStack(spacing: 10) {
                Button(primaryActionTitle) {
                    onPrimaryAction()
                }
                .buttonStyle(.borderedProminent)

                if let onSecondaryAction {
                    Button("Join") {
                        onSecondaryAction()
                    }
                    .buttonStyle(.bordered)
                }
            }
        }
        .mobileShellCard()
    }
}

private struct MobileWorkspaceMiniCard: View {
    let waddle: WaddleSummary
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            VStack(alignment: .leading, spacing: 10) {
                MobileWorkspaceAvatar(name: waddle.name, size: 40)

                Text(waddle.name)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(2)

                Text("Switch")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
            }
            .frame(width: 128, alignment: .leading)
            .mobileShellCard(padding: 16)
        }
        .buttonStyle(.plain)
    }
}

private struct MobileChannelPill: View {
    let title: String
    let subtitle: String?
    let isSelected: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("#\(title)")
                .font(.subheadline.weight(.semibold))
            if let subtitle, !subtitle.isEmpty {
                Text(subtitle.capitalized)
                    .font(.caption2)
            }
        }
        .foregroundStyle(isSelected ? .primary : .secondary)
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(isSelected ? Color.accentColor.opacity(0.18) : Color.secondary.opacity(0.12), in: Capsule())
    }
}

private struct MobileEmptyCard: View {
    let title: String
    let message: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.headline)
            Text(message)
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
        .mobileShellCard()
    }
}

private struct MobileWorkspaceAvatar: View {
    let name: String
    let size: CGFloat

    var body: some View {
        Text(initials)
            .font(.system(size: size * 0.36, weight: .bold, design: .rounded))
            .foregroundStyle(.primary)
            .frame(width: size, height: size)
            .background(
                LinearGradient(
                    colors: [
                        Color.accentColor.opacity(0.22),
                        Color(.secondarySystemBackground)
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                ),
                in: RoundedRectangle(cornerRadius: size * 0.34, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: size * 0.34, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.18), lineWidth: 1)
            )
    }

    private var initials: String {
        let parts = name
            .split(whereSeparator: \.isWhitespace)
            .prefix(2)
            .compactMap { $0.first.map(String.init) }

        return parts.isEmpty ? "#" : parts.joined().uppercased()
    }
}

private struct MobileShellBackground: View {
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        ZStack {
            baseColor
                .ignoresSafeArea()

            LinearGradient(
                colors: gradientColors,
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
            .opacity(reduceTransparency ? 0.55 : 0.9)
            .ignoresSafeArea()

            if !reduceTransparency {
                Circle()
                    .fill(Color.accentColor.opacity(colorScheme == .dark ? 0.18 : 0.1))
                    .frame(width: 260, height: 260)
                    .blur(radius: 70)
                    .offset(x: -120, y: -250)

                Circle()
                    .fill(Color.white.opacity(colorScheme == .dark ? 0.08 : 0.18))
                    .frame(width: 220, height: 220)
                    .blur(radius: 90)
                    .offset(x: 150, y: -200)
            }
        }
    }

    private var baseColor: Color {
        colorScheme == .dark ? Color.black : Color(.systemGroupedBackground)
    }

    private var gradientColors: [Color] {
        if colorScheme == .dark {
            return [
                Color(.systemGray6).opacity(0.22),
                Color.accentColor.opacity(0.08),
                Color.black
            ]
        }

        return [
            Color.accentColor.opacity(0.06),
            Color(.systemBackground),
            Color(.secondarySystemBackground)
        ]
    }
}

private struct MobileShellCardModifier: ViewModifier {
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    let padding: CGFloat

    func body(content: Content) -> some View {
        content
            .padding(padding)
            .background(cardFill, in: RoundedRectangle(cornerRadius: 24, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 24, style: .continuous)
                    .strokeBorder(cardStroke, lineWidth: 1)
            )
            .shadow(color: shadowColor, radius: colorScheme == .dark ? 18 : 12, y: 10)
    }

    private var cardFill: AnyShapeStyle {
        if reduceTransparency {
            return AnyShapeStyle(colorScheme == .dark ? Color.white.opacity(0.08) : Color.white.opacity(0.94))
        }

        return AnyShapeStyle(colorScheme == .dark ? .thinMaterial : .regularMaterial)
    }

    private var cardStroke: Color {
        colorScheme == .dark ? Color.white.opacity(0.08) : Color.white.opacity(0.55)
    }

    private var shadowColor: Color {
        colorScheme == .dark ? Color.black.opacity(0.28) : Color.black.opacity(0.08)
    }
}

private extension View {
    func mobileShellCard(padding: CGFloat = 18) -> some View {
        modifier(MobileShellCardModifier(padding: padding))
    }

    func mobileStatPill() -> some View {
        font(.footnote.weight(.medium))
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(.quaternary.opacity(0.75), in: Capsule())
    }
}
#endif
