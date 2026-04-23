#if os(macOS)
import AppKit
import SwiftUI

struct DesktopAuthenticatedShell: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    let onShowSettings: () -> Void
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 0) {
            DesktopWorkspaceRail(
                model: model,
                showCreateSheet: $showCreateSheet,
                onShowSettings: onShowSettings
            )

            Divider()
                .overlay(WaddleTheme.divider)

            DesktopWorkspaceStage(model: model)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(shellBackdrop.ignoresSafeArea())
        .sheet(isPresented: $showCreateSheet) {
            CreateWaddleSheet(model: model)
        }
    }

    private var shellBackdrop: some View {
        ZStack {
            WaddleTheme.chatBackground

            LinearGradient(
                colors: [
                    Color.accentColor.opacity(colorScheme == .dark ? 0.07 : 0.03),
                    Color.clear,
                    WaddleTheme.sidebarBackground.opacity(0.7),
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
    }
}

private struct DesktopWorkspaceRail: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    let onShowSettings: () -> Void
    @Environment(\.colorScheme) private var colorScheme

    private var orderedWaddles: [WaddleSummary] {
        model.publicWaddles.sorted { lhs, rhs in
            let lhsSelected = lhs.id == model.selectedWaddleID
            let rhsSelected = rhs.id == model.selectedWaddleID
            if lhsSelected != rhsSelected {
                return lhsSelected
            }

            let lhsJoined = model.isJoined(lhs.id)
            let rhsJoined = model.isJoined(rhs.id)
            if lhsJoined != rhsJoined {
                return lhsJoined && !rhsJoined
            }

            return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 14) {
                WaddleBrandMark(size: 42)
                    .frame(width: 46, height: 46)
                    .background(
                        RoundedRectangle(cornerRadius: 16, style: .continuous)
                            .fill(WaddleTheme.surfaceRaised)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 16, style: .continuous)
                            .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                    }

                Rectangle()
                    .fill(WaddleTheme.divider)
                    .frame(width: 28, height: 1)
            }
            .padding(.top, 18)
            .padding(.bottom, 14)

            ScrollView(.vertical, showsIndicators: false) {
                VStack(spacing: 12) {
                    if orderedWaddles.isEmpty {
                        Text("No\nwaddles")
                            .font(.system(size: 10, weight: .semibold))
                            .multilineTextAlignment(.center)
                            .foregroundStyle(WaddleTheme.textMuted)
                            .padding(.top, 10)
                    } else {
                        ForEach(orderedWaddles.prefix(12)) { waddle in
                            DesktopWorkspaceAvatarButton(
                                waddle: waddle,
                                isSelected: model.selectedWaddleID == waddle.id,
                                isJoined: model.isJoined(waddle.id)
                            ) {
                                Task { await model.selectWaddle(waddle.id) }
                            }
                        }
                    }
                }
                .padding(.vertical, 4)
            }

            Spacer(minLength: 12)

            VStack(spacing: 10) {
                DesktopRailIconButton(systemName: "plus", accessibilityLabel: "Create workspace") {
                    showCreateSheet = true
                }

                DesktopRailIconButton(systemName: "gearshape", accessibilityLabel: "Settings") {
                    onShowSettings()
                }

                if let session = model.session {
                    Button(action: onShowSettings) {
                        Text(initials(for: session.username))
                            .font(.system(size: 13, weight: .bold, design: .rounded))
                            .foregroundStyle(Color.accentColor)
                            .frame(width: 42, height: 42)
                            .background(
                                Circle()
                                    .fill(Color.accentColor.opacity(colorScheme == .dark ? 0.18 : 0.12))
                            )
                            .overlay {
                                Circle()
                                    .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                            }
                    }
                    .buttonStyle(.plain)
                    .help(session.username)
                    .contextMenu {
                        Button("Settings", action: onShowSettings)
                        Button("Sign out") {
                            Task { await model.signOut() }
                        }
                    }
                }
            }
            .padding(.bottom, 16)
        }
        .frame(width: 78)
        .frame(maxHeight: .infinity)
        .background(railBackground)
    }

    private var railBackground: some View {
        ZStack {
            WaddleTheme.railBackground

            LinearGradient(
                colors: [
                    Color.accentColor.opacity(colorScheme == .dark ? 0.06 : 0.025),
                    Color.clear,
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
    }

    private func initials(for value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }
}

private struct DesktopWorkspaceAvatarButton: View {
    let waddle: WaddleSummary
    let isSelected: Bool
    let isJoined: Bool
    let action: () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            ZStack(alignment: .bottomTrailing) {
                Text(initials)
                    .font(.system(size: 15, weight: .bold, design: .rounded))
                    .foregroundStyle(isSelected ? Color.white : WaddleTheme.textPrimary)
                    .frame(width: 46, height: 46)
                    .background(
                        RoundedRectangle(cornerRadius: 16, style: .continuous)
                            .fill(tileFill)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 16, style: .continuous)
                            .strokeBorder(tileBorder, lineWidth: 1)
                    }

                if isJoined {
                    Circle()
                        .fill(WaddleTheme.presenceOnline)
                        .frame(width: 11, height: 11)
                        .overlay {
                            Circle()
                                .strokeBorder(WaddleTheme.railBackground, lineWidth: 2)
                        }
                        .offset(x: 2, y: 2)
                }
            }
        }
        .buttonStyle(.plain)
        .help(waddle.name)
        .onHover { hovering in
            isHovering = hovering
        }
    }

    private var initials: String {
        let parts = waddle.name.split(whereSeparator: \.isWhitespace).prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }

    private var tileFill: AnyShapeStyle {
        if isSelected {
            return AnyShapeStyle(
                LinearGradient(
                    colors: [
                        Color.accentColor.opacity(colorScheme == .dark ? 0.92 : 0.86),
                        Color.accentColor.opacity(colorScheme == .dark ? 0.80 : 0.72),
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
            )
        }

        return AnyShapeStyle(isHovering ? WaddleTheme.surfaceHover : WaddleTheme.surfaceRaised)
    }

    private var tileBorder: Color {
        if isSelected {
            return Color.accentColor.opacity(colorScheme == .dark ? 0.44 : 0.26)
        }

        return WaddleTheme.divider
    }
}

private struct DesktopRailIconButton: View {
    let systemName: String
    let accessibilityLabel: String
    let action: () -> Void
    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(WaddleTheme.textPrimary)
                .frame(width: 42, height: 42)
                .background(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .fill(isHovering ? WaddleTheme.surfaceHover : WaddleTheme.surfaceRaised)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                }
        }
        .buttonStyle(.plain)
        .help(accessibilityLabel)
        .accessibilityLabel(accessibilityLabel)
        .onHover { hovering in
            isHovering = hovering
        }
    }
}

private struct DesktopWorkspaceStage: View {
    @ObservedObject var model: AppModel

    var body: some View {
        WaddleDetailView(model: model)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
    }
}

struct DesktopActionButton: View {
    let systemName: String
    let accessibilityLabel: String
    let action: () -> Void
    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(WaddleTheme.textPrimary)
                .frame(width: 32, height: 32)
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(isHovering ? WaddleTheme.surfaceHover : WaddleTheme.surfaceRaised)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                }
        }
        .buttonStyle(.plain)
        .help(accessibilityLabel)
        .accessibilityLabel(accessibilityLabel)
        .onHover { hovering in
            isHovering = hovering
        }
    }
}

#endif
