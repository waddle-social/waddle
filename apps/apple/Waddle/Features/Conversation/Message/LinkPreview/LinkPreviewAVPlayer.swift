import AVKit
import Combine
import SwiftUI
import WaddleKit

/// The card mounts this player only after a press.
struct LinkPreviewAVPlayer: View {
    let video: LinkPreviewVideo
    let muted: Bool
    let onFailure: () -> Void
    @State private var player: AVPlayer?

    var body: some View {
        ZStack {
            if let player, let item = player.currentItem {
                VideoPlayer(player: player)
                    .onReceive(player.publisher(for: \.status).receive(on: RunLoop.main)) { status in
                        if status == .failed { onFailure() }
                    }
                    .onReceive(item.publisher(for: \.status).receive(on: RunLoop.main)) { status in
                        if status == .failed { onFailure() }
                    }
                    .onReceive(NotificationCenter.default.publisher(
                        for: AVPlayerItem.failedToPlayToEndTimeNotification, object: item
                    ).receive(on: RunLoop.main)) { _ in
                        onFailure()
                    }
                    .onAppear { player.play() }
            } else {
                ProgressView()
            }
        }
        .onAppear {
            // A stream's declared type identifies playlists whose URLs have no extension.
            let options = video.isHLS ? [AVURLAssetOverrideMIMETypeKey: "application/vnd.apple.mpegurl"] : nil
            let asset = AVURLAsset(url: video.url, options: options)
            let player = AVPlayer(playerItem: AVPlayerItem(asset: asset))
            player.isMuted = muted
            self.player = player
        }
        .onDisappear {
            player?.pause()
            player?.replaceCurrentItem(with: nil)
            player = nil
        }
    }
}
