import WaddleKit

/// One layout supplies both the row's rich text and its room actions.
struct MessageBodyPresentation {
    let blocks: [RichBlock]
    let roomLinks: MessageRoomLinks

    init?(item: TimelineItem, account: AccountIdentity, channels: [Channel], actor: String) {
        guard item.tombstone == nil, MessageContent.visibleBody(of: item) != nil,
              MessageMedia.inlineImageURL(of: item) == nil else { return nil }
        let rendered = RichTextLayout.blocks(for: item, account: account, detectLinks: MessageLinkDetector.links(in:))
        blocks = MeAction.hiddenScalarCount(ofBody: item.body) > 0
            ? MeActionLine.blocks(rendered, actor: actor)
            : rendered
        roomLinks = MessageRoomLinks(blocks: blocks, channels: channels)
    }
}

/// Laying a body out (blocks, detected links, room links) is the costly part of a row. A row's
/// own view is evaluated again for a hover or for a highlight elsewhere, and must not lay the
/// body out again then: the layout is kept until one of its inputs changes.
final class MessageBodyPresentationMemo {
    private struct Inputs: Equatable {
        let item: TimelineItem
        let account: AccountIdentity
        let channels: [Channel]
        let actor: String
    }

    private var inputs: Inputs?
    private var presentation: MessageBodyPresentation?

    func presentation(
        item: TimelineItem, account: AccountIdentity, channels: [Channel], actor: String
    ) -> MessageBodyPresentation? {
        let inputs = Inputs(item: item, account: account, channels: channels, actor: actor)
        if inputs != self.inputs {
            self.inputs = inputs
            presentation = MessageBodyPresentation(item: item, account: account, channels: channels, actor: actor)
        }
        return presentation
    }
}

extension MessageRoomLinks {
    @MainActor
    static func open(_ room: BareJID, using navigation: NavigationModel, channels: [Channel]) {
        guard let room = RoomLinks.availableRoom(room, in: channels) else { return }
        navigation.open(.room(room))
    }
}
