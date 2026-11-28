/// The current selection and attendance totals shown on an agenda card.
public struct EventRSVP: Sendable {
    public let selection: EventReply?
    public let countsLabel: String

    public init(event: CommunityEvent, viewer: BareJID) {
        selection = event.reply(of: viewer)
        let going = event.attendees(replying: .going).count
        let maybe = event.attendees(replying: .maybe).count
        let declined = event.attendees(replying: .declined).count
        countsLabel = "\(going) going · \(maybe) maybe · \(declined) can't"
    }

    public static func choiceLabel(for reply: EventReply) -> String {
        switch reply {
        case .going: "Going"
        case .maybe: "Maybe"
        case .declined: "Can't"
        }
    }
}
