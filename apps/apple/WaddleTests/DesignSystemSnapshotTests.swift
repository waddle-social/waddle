import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

/// Night and Daylight goldens for every design-system component.
///
/// To re-record after an intended change, delete the affected files under
/// `__Snapshots__/DesignSystemSnapshotTests` and run the tests once: a
/// missing golden is written and the test fails; the next run compares.
@MainActor
final class DesignSystemSnapshotTests: XCTestCase {
    func testColors() {
        typealias Colors = Theme.Colors
        let swatches: [(String, Color)] = [
            ("bg", Colors.bg), ("surface", Colors.surface), ("surface2", Colors.surface2),
            ("border", Colors.border), ("fg", Colors.fg), ("fgMuted", Colors.fgMuted),
            ("action", Colors.action), ("live", Colors.live), ("gold", Colors.gold),
            ("danger", Colors.danger), ("warning", Colors.warning), ("success", Colors.success),
            ("info", Colors.info), ("focus", Colors.focus), ("sea", Colors.sea),
        ]
        assertHuddleSnapshot(of: LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: 3), spacing: Theme.Spacing.m) {
            ForEach(swatches, id: \.0) { name, color in
                VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                    RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                        .fill(color)
                        .frame(height: 44)
                        .overlay(
                            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                                .strokeBorder(Colors.borderSoft)
                        )
                    Text(name)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Colors.fgMuted)
                }
            }
        })
    }

    func testTypography() {
        typealias Typography = Theme.Typography
        let styles: [(String, HuddleTextStyle)] = [
            ("Huddle up, anywhere.", Typography.display),
            ("What you get today", Typography.headline),
            ("release-train", Typography.title),
            ("Design critique", Typography.subtitle),
            ("Server tests are green on main. Tagging the RC after the huddle.", Typography.message),
            ("Open-source rooms, huddles and discussions on an open standard.", Typography.body),
            ("Mika", Typography.author),
            ("Rooms", Typography.sidebarSection),
            ("Mika · 13:48 · 3 replies", Typography.caption),
            ("let room = try await session.join(jid)", Typography.code),
        ]
        assertHuddleSnapshot(of: VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            ForEach(styles, id: \.0) { sample, style in
                Text(sample)
                    .huddleText(style)
                    .foregroundStyle(Theme.Colors.fg)
            }
        })
    }

    func testButtons() {
        assertHuddleSnapshot(of: VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            HStack(spacing: Theme.Spacing.s) {
                Button("Join huddle") {}.buttonStyle(.huddle(.primary))
                Button("Invite") {}.buttonStyle(.huddle())
                Button("Live now") {}.buttonStyle(.huddle(.live))
            }
            HStack(spacing: Theme.Spacing.s) {
                Button("Leave room") {}.buttonStyle(.huddle(.danger))
                Button("Not now") {}.buttonStyle(.huddle()).disabled(true)
            }
            HStack(spacing: Theme.Spacing.s) {
                Button("Small") {}.buttonStyle(.huddle(.primary, size: .small))
                Button("Regular") {}.buttonStyle(.huddle(.primary))
                Button("Large") {}.buttonStyle(.huddle(.primary, size: .large))
            }
            Button {} label: {
                Label("New room", systemImage: "plus")
            }
            .buttonStyle(.huddle())
        })
    }

    func testChipsTagsAndBadges() {
        assertHuddleSnapshot(of: VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            HStack(spacing: Theme.Spacing.s) {
                Button("🎉 4") {}.buttonStyle(.huddleChip(isSelected: true))
                Button("👀 2") {}.buttonStyle(.huddleChip())
                Button("Following") {}.buttonStyle(.huddleChip())
            }
            HStack(spacing: Theme.Spacing.s) {
                HuddleTag("Owner")
                HuddleTag("Bot")
                HuddleTag("Forum")
            }
            HStack(spacing: Theme.Spacing.s) {
                UnreadBadge(count: 3, isMention: true)
                UnreadBadge(count: 12)
                UnreadBadge(count: 140, isMention: true)
            }
        })
    }

    func testSegmentedControl() {
        assertHuddleSnapshot(of: VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            HuddleSegmentedControl(
                selection: .constant("gallery"),
                segments: [.init(value: "gallery", title: "Gallery"), .init(value: "speaker", title: "Speaker")]
            )
            HuddleSegmentedControl(
                selection: .constant("unread"),
                segments: [
                    .init(value: "all", title: "All", systemImage: "tray"),
                    .init(value: "unread", title: "Unread", systemImage: "circle.fill"),
                    .init(value: "following", title: "Following", systemImage: "star"),
                ]
            )
        })
    }

    func testToggles() {
        assertHuddleSnapshot(of: VStack(spacing: Theme.Spacing.m) {
            Toggle("Read receipts", isOn: .constant(true))
            Toggle("Compact messages", isOn: .constant(false))
            Toggle("Notification previews", isOn: .constant(true)).disabled(true)
        }
        .toggleStyle(.huddle)
        .huddleText(Theme.Typography.body)
        .foregroundStyle(Theme.Colors.fg))
    }

    func testAvatarsAndPresence() {
        let people = ["Ana Lindqvist", "Jo Hartmann", "Mika", "Priya", "Theo", "Åse Ørn"]
        let states: [Availability] = [.available, .away, .doNotDisturb, .offline]
        assertHuddleSnapshot(of: VStack(alignment: .leading, spacing: Theme.Spacing.l) {
            HStack(spacing: Theme.Spacing.s) {
                ForEach(people, id: \.self) { AvatarView(name: $0) }
            }
            HStack(alignment: .bottom, spacing: Theme.Spacing.m) {
                AvatarView(name: "Mika", size: Theme.Size.smallAvatar)
                AvatarView(name: "Mika", size: Theme.Size.rowAvatar)
                AvatarView(name: "Mika", size: Theme.Size.avatar)
                AvatarView(name: "Mika", size: 64)
            }
            HStack(spacing: Theme.Spacing.l) {
                ForEach(states, id: \.self) { state in
                    AvatarView(name: "Jo Hartmann")
                        .overlay(alignment: .bottomTrailing) {
                            PresenceDot(availability: state, size: 14).offset(x: 2, y: 2)
                        }
                }
            }
            HStack(spacing: Theme.Spacing.l) {
                ForEach(states, id: \.self) { PresenceDot(availability: $0, size: 20) }
            }
        })
    }

    func testSurfaces() {
        assertHuddleSnapshot(of: VStack(alignment: .leading, spacing: Theme.Spacing.xl) {
            ForEach(Array(zip(["Cards", "Menus, hover", "Dialogs"], HuddleElevation.allCases)), id: \.0) { title, elevation in
                Text(title)
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fg)
                    .padding(Theme.Spacing.l)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .huddleSurface(elevation)
            }
            HuddleCard(isLive: true) {
                Text("Huddle in release-train")
                    .huddleText(Theme.Typography.subtitle)
                    .foregroundStyle(Theme.Colors.fg)
                Text("Mika, Jo and 2 others")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        }
        .padding(Theme.Spacing.s))
    }

    func testBannersAndEmptyState() {
        // Three detailed SF Symbols: allow for their redraws between OS releases.
        assertHuddleSnapshot(precision: 0.95, of: VStack(spacing: Theme.Spacing.l) {
            WaddleBrandMark(size: 72)
            ConnectionBanner(status: .offline(retryAt: nil))
            ConnectionBanner(status: .authenticationFailed)
            EmptyStateView(
                title: "Pick a conversation",
                message: "Choose a room or a direct message to start reading.",
                symbol: "bubble.left.and.bubble.right"
            )
            .frame(height: 220)
        })
    }
}
