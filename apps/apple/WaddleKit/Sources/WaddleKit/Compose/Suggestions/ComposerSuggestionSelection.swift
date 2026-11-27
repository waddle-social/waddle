/// The visible rows and their keyboard selection, scoped to one typed
/// trigger. Escape keeps that trigger closed through late search results.
public struct ComposerSuggestionSelection: Equatable, Sendable {
    public enum Direction: Sendable {
        case up
        case down
    }

    /// The interaction state carried across presentations. A navigation
    /// without a cursor is an explicit dismissal of that trigger.
    public struct Navigation: Equatable, Sendable {
        fileprivate let trigger: ComposerSuggestionTrigger
        fileprivate let cursor: Cursor?
    }

    fileprivate struct Cursor: Equatable, Sendable {
        let item: ComposerSuggestion
        let index: Int
    }

    public private(set) var trigger: ComposerSuggestionTrigger?
    public private(set) var items: [ComposerSuggestion] = []
    public private(set) var navigation: Navigation?

    public init(navigation: Navigation? = nil) {
        self.navigation = navigation
    }

    public var selectedIndex: Int? {
        items.isEmpty ? nil : navigation?.cursor?.index
    }

    public var selected: ComposerSuggestion? {
        selectedIndex.map { items[$0] }
    }

    public func isDismissed(_ trigger: ComposerSuggestionTrigger) -> Bool {
        navigation?.trigger == trigger && navigation?.cursor == nil
    }

    /// A new trigger starts at the first row. Result updates follow the
    /// selected item, falling back to the nearest valid index if it vanished.
    public mutating func update(trigger: ComposerSuggestionTrigger?, items: [ComposerSuggestion]) {
        self.trigger = trigger
        if navigation?.trigger != trigger { navigation = nil }
        self.items = []
        guard let trigger, !isDismissed(trigger) else { return }
        self.items = items
        guard !items.isEmpty else {
            navigation = nil
            return
        }
        let index = navigation?.cursor.map { cursor in
            items.firstIndex(of: cursor.item) ?? min(cursor.index, items.count - 1)
        } ?? 0
        select(index, for: trigger)
    }

    public mutating func move(_ direction: Direction) {
        guard let trigger, let selectedIndex else { return }
        let step = direction == .up ? -1 : 1
        select((selectedIndex + step + items.count) % items.count, for: trigger)
    }

    public mutating func dismiss() {
        guard let trigger else { return }
        navigation = Navigation(trigger: trigger, cursor: nil)
        items = []
    }

    private mutating func select(_ index: Int, for trigger: ComposerSuggestionTrigger) {
        navigation = Navigation(trigger: trigger, cursor: Cursor(item: items[index], index: index))
    }
}
