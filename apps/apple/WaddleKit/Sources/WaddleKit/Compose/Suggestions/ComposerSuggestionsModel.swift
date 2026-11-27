/// Keys the composer can take from the platform text system.
public enum ComposerHardwareKey: Sendable {
    case tab
    case returnKey
    case escape
    case up
    case down
}

/// A presentation of the suggestions for the current draft and selection.
public struct ComposerSuggestionsModel: Sendable {
    public struct DirectoryQuery: Hashable, Sendable {
        public let room: BareJID
        public let text: String
    }

    public struct DirectoryResults: Sendable {
        public let query: DirectoryQuery
        public let users: [UserSearchResult]

        public init(query: DirectoryQuery, users: [UserSearchResult]) {
            self.query = query
            self.users = users
        }
    }

    public struct Pick: Equatable, Sendable {
        public let edit: ComposerTextEdit
        public let recordedMention: RecordedMention?
        public let clearsError: Bool
    }

    public enum KeyAction: Equatable, Sendable {
        case pass
        case navigate(ComposerSuggestionSelection.Navigation?)
        case pick(Pick)
    }

    public let list: ComposerSuggestionSelection
    public let directoryQuery: DirectoryQuery?
    private let slashResolves: Bool

    /// The occupants and the extensions are read only for the trigger that needs them: a composer
    /// with no list open must not come to depend on every room's presence.
    public static func present(
        text: String, selection: ComposerScalarRange?, isEditing: Bool, conversation: ConversationID,
        inChannel: Bool, occupants: @autoclosure () -> [Occupant], account: AccountIdentity,
        extensions: @autoclosure () -> [ExtensionCommand],
        directory: DirectoryResults?, navigation: ComposerSuggestionSelection.Navigation?
    ) -> Self {
        let token = ComposerSuggestionTrigger.find(in: text, selection: selection)
        let trigger = isEditing && token?.kind != .emoji ? nil : token
        var list = ComposerSuggestionSelection(navigation: navigation)
        guard let trigger, !list.isDismissed(trigger) else {
            list.update(trigger: trigger, items: [])
            return Self(list: list, directoryQuery: nil, slashResolves: false)
        }

        let items: [ComposerSuggestion]
        var directoryQuery: DirectoryQuery?
        var slashResolves = false
        switch trigger.kind {
        case .mention where conversation.isRoom:
            if !trigger.query.isEmpty {
                directoryQuery = DirectoryQuery(room: conversation.jid, text: trigger.query)
            }
            items = MentionCandidates.matching(
                trigger.query, occupants: occupants(), ownNick: account.nick,
                directoryUsers: directory?.query == directoryQuery ? directory?.users ?? [] : [],
                ownJID: account.jid
            ).map(ComposerSuggestion.mention)
        case .mention:
            items = []
        case .emoji:
            items = EmojiShortcodes.search(trigger.query).map(ComposerSuggestion.emoji)
        case .slash:
            let extensions = extensions()
            items = SlashCandidates.filter(prefix: trigger.query, extensions: extensions, inRoom: inChannel)
                .prefix(ComposerSuggestion.maximumRows).map(ComposerSuggestion.slash)
            if let command = SlashTrigger.parse(text) {
                slashResolves = SlashCandidates.resolve(prefix: command.prefix, extensions: extensions, inRoom: inChannel) != nil
            }
        }

        list.update(trigger: trigger, items: items)
        return Self(list: list, directoryQuery: directoryQuery, slashResolves: slashResolves)
    }

    public func handle(_ key: ComposerHardwareKey, in text: String) -> KeyAction {
        var list = list
        guard let item = list.selected, let trigger = list.trigger else { return .pass }
        switch key {
        case .up: list.move(.up)
        case .down: list.move(.down)
        case .escape: list.dismiss()
        case .returnKey where slashResolves: return .pass
        case .tab, .returnKey:
            return .pick(Self.makePick(item, trigger: trigger, text: text))
        }
        return .navigate(list.navigation)
    }

    /// A pointer event can arrive after its trigger or result has gone away.
    public func pick(_ item: ComposerSuggestion, from trigger: ComposerSuggestionTrigger, in text: String) -> Pick? {
        guard list.trigger == trigger, list.items.contains(item) else { return nil }
        return Self.makePick(item, trigger: trigger, text: text)
    }

    private static func makePick(_ item: ComposerSuggestion, trigger: ComposerSuggestionTrigger, text: String) -> Pick {
        Pick(edit: item.accept(in: text, replacing: trigger.replacementRange),
             recordedMention: item.recordedMention, clearsError: item.kind == .slash)
    }
}
