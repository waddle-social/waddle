#if os(macOS)
import AppKit
import SwiftUI

struct DesktopAuthenticatedShell: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    let onShowSettings: () -> Void
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        ZStack(alignment: .topTrailing) {
            DesktopSpaceStage(model: model)
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            HStack(spacing: 10) {
                if model.isLoadingStructure {
                    ProgressView()
                }

                DesktopShellIconButton(systemName: "plus", accessibilityLabel: "Create space") {
                    showCreateSheet = true
                }

                DesktopShellIconButton(systemName: "gearshape", accessibilityLabel: "Settings") {
                    onShowSettings()
                }
            }
            .padding(18)
        }
        .background(shellBackdrop.ignoresSafeArea())
        .sheet(isPresented: $showCreateSheet) {
            CreateSpaceSheet(model: model)
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

private struct DesktopShellIconButton: View {
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

private struct DesktopSpaceStage: View {
    @ObservedObject var model: AppModel

    var body: some View {
        WaddleDetailView(model: model)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
    }
}

#endif
