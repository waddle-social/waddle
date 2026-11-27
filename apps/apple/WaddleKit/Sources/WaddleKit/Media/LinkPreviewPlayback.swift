import Foundation

public enum LinkPreviewPlayback: Hashable, Sendable {
    case video(LinkPreviewVideo)
    case embed(LinkPreviewEmbed)

    public init?(video: LinkPreviewVideo?, embed: LinkPreviewEmbed?) {
        if let video {
            self = .video(video)
        } else if let embed {
            self = .embed(embed)
        } else {
            return nil
        }
    }
}

/// A card owns this state and advances it only in response to its controls and player.
public enum LinkPreviewCardState: Equatable, Sendable {
    case link
    case ready(LinkPreviewPlayback)
    case video(LinkPreviewVideo, muted: Bool)
    case embed(LinkPreviewEmbed)
    case fallback

    public init(playback: LinkPreviewPlayback?) {
        self = playback.map(Self.ready) ?? .link
    }

    public mutating func play(muted: Bool) {
        switch self {
        case let .ready(.video(video)):
            self = .video(video, muted: muted)
        case let .ready(.embed(embed)):
            if let player = embed.forPlayback(muted: muted) {
                self = .embed(player)
            } else {
                self = .fallback
            }
        case .link, .video, .embed, .fallback:
            break
        }
    }

    public mutating func fail() {
        switch self {
        case .video, .embed: self = .fallback
        case .link, .ready, .fallback: break
        }
    }

    public mutating func stop(playback: LinkPreviewPlayback?) {
        switch self {
        case .video, .embed: self = Self(playback: playback)
        case .link, .ready, .fallback: break
        }
    }
}
