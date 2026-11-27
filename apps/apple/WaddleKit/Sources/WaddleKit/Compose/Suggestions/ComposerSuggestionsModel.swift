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
    public struct Draft: Sendable {
        let text: String
        let selection: ComposerScalarRange?
        let isEditing: Bool
        let roomLinks: [RoomLinkDraft]

        public init(text: String, selection: ComposerScalarRange?, isEditing: Bool, roomLinks: [RoomLinkDraft] = []) {
            self.text = text
            self.selection = selection
            self.isEditing = isEditing
            self.roomLinks = roomLinks
        }
    }

    public struct Place: Sendable {
        let conversation: ConversationID
        let inChannel: Bool
        let account: AccountIdentity

        public init(conversation: ConversationID, inChannel: Bool, account: AccountIdentity) {
            self.conversation = conversation
            self.inChannel = inChannel
            self.account = account
        }
    }

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
        public let replacementRange: ComposerScalarRange
        public let reference: RecordedDraftReference?
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

    /// Read each list's data only for the trigger that needs it.
    public static func present(
        draft: Draft, place: Place, occupants: @autoclosure () -> [Occupant],
        extensions: @autoclosure () -> [ExtensionCommand],
        directory: DirectoryResults?, navigation: ComposerSuggestionSelection.Navigation?,
        rooms: @autoclosure () -> [Channel] = []
    ) -> Self {
        var token = ComposerSuggestionTrigger.find(
            in: draft.text, selection: draft.selection, roomLinks: draft.roomLinks, includesRooms: !draft.isEditing
        )
        var matchingRooms: [RoomLinks.Candidate] = []
        if let roomTrigger = token, roomTrigger.kind == .room {
            matchingRooms = RoomLinks.search(RoomLinks.candidates(from: rooms()), for: roomTrigger.query)
            if matchingRooms.isEmpty {
                token = ComposerSuggestionTrigger.find(
                    in: draft.text, selection: draft.selection, roomLinks: [], includesRooms: false
                )
            }
        }
        let trigger = draft.isEditing && token?.kind != .emoji ? nil : token
        var list = ComposerSuggestionSelection(navigation: navigation)
        guard let trigger, !list.isDismissed(trigger) else {
            list.update(trigger: trigger, items: [])
            return Self(list: list, directoryQuery: nil, slashResolves: false)
        }

        let items: [ComposerSuggestion]
        var directoryQuery: DirectoryQuery?
        var slashResolves = false
        switch trigger.kind {
        case .mention where place.conversation.isRoom:
            if !trigger.query.isEmpty {
                directoryQuery = DirectoryQuery(room: place.conversation.jid, text: trigger.query)
            }
            items = MentionCandidates.matching(
                trigger.query, occupants: occupants(), ownNick: place.account.nick,
                directoryUsers: directory?.query == directoryQuery ? directory?.users ?? [] : [],
                ownJID: place.account.jid
            ).map(ComposerSuggestion.mention)
        case .mention:
            items = []
        case .room:
            items = matchingRooms.map(ComposerSuggestion.room)
        case .emoji:
            items = EmojiShortcodes.search(trigger.query).map(ComposerSuggestion.emoji)
        case .slash:
            let extensions = extensions()
            items = SlashCandidates.filter(prefix: trigger.query, extensions: extensions, inRoom: place.inChannel)
                .prefix(ComposerSuggestion.maximumRows).map(ComposerSuggestion.slash)
            if let command = SlashTrigger.parse(draft.text) {
                slashResolves = SlashCandidates.resolve(
                    prefix: command.prefix, extensions: extensions, inRoom: place.inChannel
                ) != nil
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
        case .returnKey:
            return pickForSubmit(in: text).map(KeyAction.pick) ?? .pass
        case .tab:
            return .pick(Self.makePick(item, trigger: trigger, text: text))
        }
        return .navigate(list.navigation)
    }

    /// Send and Return complete the highlighted row unless slash dispatch
    /// already has an exact command to run.
    public func pickForSubmit(in text: String) -> Pick? {
        guard !slashResolves, let item = list.selected, let trigger = list.trigger else { return nil }
        return Self.makePick(item, trigger: trigger, text: text)
    }

    /// A pointer event can arrive after its trigger or result has gone away.
    public func pick(_ item: ComposerSuggestion, from trigger: ComposerSuggestionTrigger, in text: String) -> Pick? {
        guard list.trigger == trigger, let visible = list.items.first(where: { $0.matchesSelection(item) }) else { return nil }
        return Self.makePick(visible, trigger: trigger, text: text)
    }

    private static func makePick(_ item: ComposerSuggestion, trigger: ComposerSuggestionTrigger, text: String) -> Pick {
        let range = trigger.replacementRange
        let reference = item.recordedMention.map(RecordedDraftReference.mention)
            ?? item.roomLink(at: range.lowerBound).map(RecordedDraftReference.roomLink)
        return Pick(edit: item.accept(in: text, replacing: range), replacementRange: range,
                    reference: reference, clearsError: item.kind == .slash)
    }
}
