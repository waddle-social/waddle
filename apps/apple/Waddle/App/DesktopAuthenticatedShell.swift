#if os(macOS)
import AppKit
import SwiftUI

struct DesktopAuthenticatedShell: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    let onShowSettings: () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var showWorkspaceSidebar = true

    var body: some View {
        HStack(spacing: 0) {
            DesktopWorkspaceRail(
                model: model,
                showWorkspaceSidebar: $showWorkspaceSidebar,
                onShowSettings: onShowSettings
            )

            if showWorkspaceSidebar {
                DesktopWorkspaceSidebar(
                    model: model,
                    showCreateSheet: $showCreateSheet,
                    onShowSettings: onShowSettings
                )
                .frame(
                    minWidth: AppConfig.desktopSidebarMinWidth,
                    idealWidth: AppConfig.desktopSidebarIdealWidth,
                    maxWidth: AppConfig.desktopSidebarMaxWidth
                )

                Divider()
                    .overlay(Color.primary.opacity(colorScheme == .dark ? 0.10 : 0.05))
            }

            DesktopWorkspaceStage(
                model: model,
                showCreateSheet: $showCreateSheet,
                onShowSettings: onShowSettings
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .animation(.easeOut(duration: 0.18), value: showWorkspaceSidebar)
        .background(shellBackdrop.ignoresSafeArea())
        .sheet(isPresented: $showCreateSheet) {
            CreateWaddleSheet(model: model)
        }
    }

    private var shellBackdrop: some View {
        ZStack {
            Color(nsColor: colorScheme == .dark ? .windowBackgroundColor : .underPageBackgroundColor)
            LinearGradient(
                colors: [
                    Color.accentColor.opacity(colorScheme == .dark ? 0.08 : 0.04),
                    Color.clear,
                    Color(nsColor: colorScheme == .dark ? .controlBackgroundColor : .windowBackgroundColor).opacity(0.55)
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
    }
}

private struct DesktopWorkspaceRail: View {
    @ObservedObject var model: AppModel
    @Binding var showWorkspaceSidebar: Bool
    let onShowSettings: () -> Void
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        VStack(spacing: 10) {
            Button {
                withAnimation(.easeOut(duration: 0.18)) {
                    showWorkspaceSidebar.toggle()
                }
            } label: {
                Image(systemName: showWorkspaceSidebar ? "sidebar.left" : "sidebar.right")
                    .font(.system(size: 12, weight: .semibold))
                    .frame(width: 30, height: 30)
                    .background(Color.primary.opacity(colorScheme == .dark ? 0.08 : 0.04), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
            }
            .buttonStyle(.plain)

            ScrollView(.vertical, showsIndicators: false) {
                VStack(spacing: 8) {
                    ForEach(model.publicWaddles.prefix(10)) { waddle in
                        let initial = waddle.name.first.map { String($0).uppercased() } ?? "?"
                        Button {
                            Task { await model.selectWaddle(waddle.id) }
                        } label: {
                            Text(initial)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(model.selectedWaddleID == waddle.id ? Color.accentColor : .secondary)
                                .frame(width: 30, height: 30)
                                .background(
                                    (model.selectedWaddleID == waddle.id
                                        ? Color.accentColor.opacity(colorScheme == .dark ? 0.20 : 0.12)
                                        : Color.primary.opacity(colorScheme == .dark ? 0.05 : 0.025)),
                                    in: RoundedRectangle(cornerRadius: 9, style: .continuous)
                                )
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.vertical, 2)
            }

            Spacer(minLength: 0)

            Button {
                Task { await model.refreshPublicWaddles() }
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 12, weight: .semibold))
                    .frame(width: 30, height: 30)
                    .background(Color.primary.opacity(colorScheme == .dark ? 0.08 : 0.04), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
            }
            .buttonStyle(.plain)

            Button(action: onShowSettings) {
                Image(systemName: "gearshape")
                    .font(.system(size: 12, weight: .semibold))
                    .frame(width: 30, height: 30)
                    .background(Color.primary.opacity(colorScheme == .dark ? 0.08 : 0.04), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 10)
        .frame(width: 46)
        .frame(maxHeight: .infinity)
        .background(Color(nsColor: colorScheme == .dark ? .underPageBackgroundColor : .controlBackgroundColor))
    }
}

private struct DesktopWorkspaceSidebar: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    let onShowSettings: () -> Void
    @Environment(\.colorScheme) private var colorScheme

    private var visibleWaddleCount: Int {
        model.publicWaddles.count
    }

    private var joinedWaddleCount: Int {
        model.joinedWaddleIDs.count
    }

    private var serverLabel: String {
        guard let host = AppConfig.normalizedServerURL(from: model.serverURLText)?.host, !host.isEmpty else {
            return model.serverURLText.replacingOccurrences(of: "https://", with: "")
        }
        return host
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
                .overlay(Color.primary.opacity(colorScheme == .dark ? 0.10 : 0.05))

            VStack(alignment: .leading, spacing: 12) {
                searchField

                HStack(spacing: 8) {
                    DesktopCountChip(value: visibleWaddleCount, label: "visible")
                    DesktopCountChip(value: joinedWaddleCount, label: "joined")
                    DesktopCountChip(value: model.channels.count, label: "channels")
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)

            Divider()
                .overlay(Color.primary.opacity(colorScheme == .dark ? 0.10 : 0.05))

            catalog

            footer
        }
        .background(sidebarBackground)
        .task {
            if model.publicWaddles.isEmpty {
                await model.refreshPublicWaddles()
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .center, spacing: 12) {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(Color.accentColor.opacity(colorScheme == .dark ? 0.20 : 0.12))
                    .frame(width: 38, height: 38)
                    .overlay {
                        Image(systemName: "bubble.left.and.bubble.right.fill")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(Color.accentColor)
                    }

                VStack(alignment: .leading, spacing: 2) {
                    Text("Waddle")
                        .font(.system(size: 21, weight: .semibold))
                    Text("Calm desktop workspace")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(.secondary)
                }

                Spacer(minLength: 12)

                if model.isLoadingWaddles {
                    ProgressView()
                        .controlSize(.small)
                }
            }

            if let session = model.session {
                HStack(alignment: .center, spacing: 10) {
                    Circle()
                        .fill(Color.accentColor.opacity(colorScheme == .dark ? 0.18 : 0.12))
                        .frame(width: 34, height: 34)
                        .overlay {
                            Text(initials(for: session.username))
                                .font(.system(size: 12, weight: .bold))
                                .foregroundStyle(Color.accentColor)
                        }

                    VStack(alignment: .leading, spacing: 2) {
                        Text(session.username)
                            .font(.system(size: 13, weight: .semibold))
                            .lineLimit(1)
                        Text(serverLabel)
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }

                    Spacer(minLength: 8)

                    DesktopActionButton(systemName: "gearshape", accessibilityLabel: "Settings") {
                        onShowSettings()
                    }
                }
            }
        }
        .padding(.horizontal, 20)
        .padding(.top, 18)
        .padding(.bottom, 16)
    }

    private var searchField: some View {
        HStack(spacing: 10) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(.secondary)

            TextField("Search public waddles", text: $model.searchQuery)
                .textFieldStyle(.plain)

            if !model.searchQuery.isEmpty {
                Button {
                    model.searchQuery = ""
                    Task { await model.refreshPublicWaddles() }
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(searchFieldBackground, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(Color.primary.opacity(colorScheme == .dark ? 0.12 : 0.06))
        }
        .onChange(of: model.searchQuery) { _, _ in
            model.schedulePublicWaddleSearch()
        }
    }

    private var catalog: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 6) {
                Text("Workspace directory")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .textCase(.uppercase)
                    .padding(.horizontal, 4)
                    .padding(.bottom, 6)

                if model.publicWaddles.isEmpty, model.isLoadingWaddles {
                    ProgressView("Loading waddles…")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 18)
                } else if model.publicWaddles.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No waddles yet")
                            .font(.system(size: 14, weight: .semibold))
                        Text("Create a workspace or broaden your search to populate the directory.")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(14)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color.primary.opacity(colorScheme == .dark ? 0.06 : 0.035), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                } else {
                    ForEach(model.publicWaddles) { waddle in
                        DesktopWaddleRow(
                            waddle: waddle,
                            isSelected: model.selectedWaddleID == waddle.id,
                            isJoined: model.isJoined(waddle.id)
                        ) {
                            Task { await model.selectWaddle(waddle.id) }
                        }
                    }
                }
            }
            .padding(16)
        }
    }

    private var footer: some View {
        VStack(spacing: 12) {
            Divider()
                .overlay(Color.primary.opacity(colorScheme == .dark ? 0.10 : 0.05))

            HStack(spacing: 8) {
                DesktopActionButton(systemName: "arrow.clockwise", accessibilityLabel: "Refresh waddles") {
                    Task { await model.refreshPublicWaddles() }
                }

                Button {
                    showCreateSheet = true
                } label: {
                    Label("New waddle", systemImage: "plus")
                        .font(.system(size: 12, weight: .semibold))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 9)
                        .background(Color.accentColor.opacity(colorScheme == .dark ? 0.18 : 0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                        .foregroundStyle(Color.accentColor)
                }
                .buttonStyle(.plain)

                Spacer(minLength: 8)

                Button {
                    Task { await model.signOut() }
                } label: {
                    Label("Sign out", systemImage: "rectangle.portrait.and.arrow.right")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 16)
        }
        .padding(.bottom, 16)
    }

    private var sidebarBackground: some View {
        ZStack {
            Color(nsColor: colorScheme == .dark ? .underPageBackgroundColor : .controlBackgroundColor)
            LinearGradient(
                colors: [
                    Color.accentColor.opacity(colorScheme == .dark ? 0.05 : 0.025),
                    Color.clear
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
    }

    private var searchFieldBackground: Color {
        Color(nsColor: colorScheme == .dark ? .windowBackgroundColor : .textBackgroundColor)
    }

    private func initials(for value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }
}

private struct DesktopWaddleRow: View {
    let waddle: WaddleSummary
    let isSelected: Bool
    let isJoined: Bool
    let action: () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Color.accentColor.opacity(isSelected ? (colorScheme == .dark ? 0.20 : 0.14) : (colorScheme == .dark ? 0.12 : 0.08)))
                    .frame(width: 34, height: 34)
                    .overlay {
                        Text(initial)
                            .font(.system(size: 13, weight: .bold))
                            .foregroundStyle(Color.accentColor)
                    }

                VStack(alignment: .leading, spacing: 5) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(waddle.name)
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundStyle(.primary)
                            .lineLimit(1)

                        if isJoined {
                            Text("Joined")
                                .font(.system(size: 10, weight: .bold))
                                .padding(.horizontal, 7)
                                .padding(.vertical, 3)
                                .background(Color.green.opacity(colorScheme == .dark ? 0.18 : 0.12), in: Capsule())
                                .foregroundStyle(Color.green.opacity(colorScheme == .dark ? 0.92 : 0.82))
                        }
                    }

                    if let description = waddle.description, !description.isEmpty {
                        Text(description)
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }

                    HStack(spacing: 8) {
                        Text((waddle.isPublic ?? true) ? "Public" : "Private")
                            .foregroundStyle(.secondary)

                        if let role = waddle.role, !role.isEmpty {
                            Circle()
                                .fill(Color.secondary.opacity(0.4))
                                .frame(width: 3, height: 3)
                            Text(role.capitalized)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .font(.system(size: 11, weight: .medium))
                }

                Spacer(minLength: 10)

                if isSelected {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(.secondary)
                        .padding(.top, 4)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(rowBackground, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .strokeBorder(Color.primary.opacity(isSelected ? (colorScheme == .dark ? 0.16 : 0.08) : (colorScheme == .dark ? 0.08 : 0.05)))
            }
        }
        .buttonStyle(.plain)
        .onHover { hovering in
            isHovering = hovering
        }
        .animation(.easeOut(duration: 0.16), value: isSelected)
    }

    private var initial: String {
        String(waddle.name.prefix(1)).uppercased()
    }

    private var rowBackground: Color {
        if isSelected {
            return Color.accentColor.opacity(colorScheme == .dark ? 0.12 : 0.08)
        }
        if isHovering {
            return Color.primary.opacity(colorScheme == .dark ? 0.07 : 0.04)
        }
        return Color.primary.opacity(colorScheme == .dark ? 0.03 : 0.02)
    }
}

private struct DesktopWorkspaceStage: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    let onShowSettings: () -> Void
    @ObservedObject private var store: ChatSurfaceStore
    @Environment(\.colorScheme) private var colorScheme

    init(model: AppModel, showCreateSheet: Binding<Bool>, onShowSettings: @escaping () -> Void) {
        self.model = model
        _showCreateSheet = showCreateSheet
        self.onShowSettings = onShowSettings
        _store = ObservedObject(wrappedValue: model.chatStore)
    }

    private var serverLabel: String {
        guard let host = AppConfig.normalizedServerURL(from: model.serverURLText)?.host, !host.isEmpty else {
            return model.serverURLText.replacingOccurrences(of: "https://", with: "")
        }
        return host
    }

    var body: some View {
        VStack(spacing: 14) {
            utilityBar

            WaddleDetailView(model: model)
        }
        .padding(12)
    }

    private var utilityBar: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(model.selectedWaddle?.name ?? "Workspace")
                    .font(.system(size: 22, weight: .semibold))
                    .lineLimit(1)

                Text(secondarySummary)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            Spacer(minLength: 12)

            if store.bannerState.isVisible {
                DesktopBannerChip(state: store.bannerState)
            }

            DesktopCountChip(value: model.channels.count, label: "channels")
            DesktopCountChip(value: model.members.count, label: "people")

            HStack(spacing: 8) {
                DesktopActionButton(systemName: "arrow.clockwise", accessibilityLabel: "Refresh waddles") {
                    Task { await model.refreshPublicWaddles() }
                }

                Button {
                    showCreateSheet = true
                } label: {
                    Label("Create", systemImage: "plus")
                        .font(.system(size: 12, weight: .semibold))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 9)
                        .background(Color.accentColor.opacity(colorScheme == .dark ? 0.18 : 0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                        .foregroundStyle(Color.accentColor)
                }
                .buttonStyle(.plain)

                DesktopActionButton(systemName: "gearshape", accessibilityLabel: "Settings") {
                    onShowSettings()
                }
            }
        }
        .padding(.horizontal, 8)
    }

    private var secondarySummary: String {
        if let channel = model.selectedChannel {
            return "#\(channel.name) · \(serverLabel)"
        }
        if model.selectedWaddle != nil {
            return "\(model.channels.count) live channels · \(serverLabel)"
        }
        return "Select a waddle from the sidebar to open live rooms on \(serverLabel)."
    }
}

private struct DesktopActionButton: View {
    let systemName: String
    let accessibilityLabel: String
    let action: () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.primary)
                .frame(width: 32, height: 32)
                .background(background, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(Color.primary.opacity(colorScheme == .dark ? 0.10 : 0.06))
                }
        }
        .buttonStyle(.plain)
        .help(accessibilityLabel)
        .accessibilityLabel(accessibilityLabel)
        .onHover { hovering in
            isHovering = hovering
        }
    }

    private var background: Color {
        if isHovering {
            return Color.primary.opacity(colorScheme == .dark ? 0.09 : 0.05)
        }
        return Color.primary.opacity(colorScheme == .dark ? 0.05 : 0.025)
    }
}

private struct DesktopCountChip: View {
    let value: Int
    let label: String
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 6) {
            Text("\(value)")
                .font(.system(size: 12, weight: .semibold))
                .monospacedDigit()
                .contentTransition(.numericText())
            Text(label)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(Color.primary.opacity(colorScheme == .dark ? 0.06 : 0.03), in: Capsule())
    }
}

private struct DesktopBannerChip: View {
    let state: ChatConnectionBannerState
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: state.symbolName)
                .font(.system(size: 11, weight: .semibold))
            Text(state.message)
                .font(.system(size: 11, weight: .medium))
                .lineLimit(1)
        }
        .foregroundStyle(foreground)
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(background, in: Capsule())
    }

    private var foreground: Color {
        switch state {
        case .connected:
            return Color.green.opacity(colorScheme == .dark ? 0.95 : 0.82)
        case .error:
            return Color.red.opacity(colorScheme == .dark ? 0.96 : 0.84)
        case .disconnected:
            return Color.orange.opacity(colorScheme == .dark ? 0.96 : 0.84)
        case .connecting, .reconnecting:
            return Color.accentColor
        case .hidden:
            return .secondary
        }
    }

    private var background: Color {
        switch state {
        case .connected:
            return Color.green.opacity(colorScheme == .dark ? 0.16 : 0.10)
        case .error:
            return Color.red.opacity(colorScheme == .dark ? 0.16 : 0.10)
        case .disconnected:
            return Color.orange.opacity(colorScheme == .dark ? 0.16 : 0.10)
        case .connecting, .reconnecting:
            return Color.accentColor.opacity(colorScheme == .dark ? 0.16 : 0.10)
        case .hidden:
            return Color.primary.opacity(colorScheme == .dark ? 0.06 : 0.03)
        }
    }
}

#endif
