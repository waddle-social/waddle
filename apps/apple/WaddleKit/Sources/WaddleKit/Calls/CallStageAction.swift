/// What someone can ask of a call's stage: every button, menu item and key of it.
public enum CallStageAction: Equatable, Sendable {
    case toggleMicrophone, toggleCamera, toggleHand, toggleScreenShare, toggleChat
    case toggleParticipants, toggleSelfView, openSettings, resetVolumes, hangUp
    case setViewMode(CallStagePresentation.ViewMode)
    case setSize(CallSurface.Size)
    case setVolume(identity: String, gain: Double)
    case react(String)
    case retry(CallMediaIssue.Kind)
    case dismissIssue(CallMediaIssue.Kind)
    case openSystemSettings(CallMediaIssue.Kind)
}
