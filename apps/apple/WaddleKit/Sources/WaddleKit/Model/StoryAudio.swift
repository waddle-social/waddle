/// The app's audio session as story playback sees it. On iPhone and iPad
/// it is `AVAudioSession`; `Setting` is its category, mode and options.
@MainActor
public protocol StoryAudioSession: AnyObject {
    associatedtype Setting
    var isAmbient: Bool { get }
    func currentSetting() -> Setting
    func setAmbient()
    func restore(_ setting: Setting)
}

/// What story playback does to the audio session while the viewer is open.
/// Outside a call stories mix with other audio and follow the silent
/// switch (ambient); the setting they replaced comes back on close. During
/// a call the session is the call's and is not touched; once the call is
/// over, ambient is set again, since the call left its own setting behind.
@MainActor
public struct StoryAudio<Session: StoryAudioSession> {
    private let session: Session
    /// What was in force before stories first changed it, to put back.
    private var saved: Session.Setting?
    /// Whether the ambient setting in force is the stories'. A call takes
    /// the session from them.
    private var ownsSession = false

    public init(session: Session) {
        self.session = session
    }

    public mutating func update(_ sound: StorySound) {
        switch sound {
        case .mixed:
            guard !(ownsSession && session.isAmbient) else { return }
            if !session.isAmbient {
                if saved == nil { saved = session.currentSetting() }
                session.setAmbient()
            }
            ownsSession = true
        case .silencedByCall:
            ownsSession = false
        }
    }

    /// The viewer closed: the saved setting comes back, unless a call has
    /// the session.
    public mutating func close() {
        defer {
            saved = nil
            ownsSession = false
        }
        guard ownsSession, session.isAmbient, let saved else { return }
        session.restore(saved)
    }
}
