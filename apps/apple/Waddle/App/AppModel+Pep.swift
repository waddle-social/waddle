import Foundation

// MARK: - PEP: Mood, Activity, Tune & Inbox

extension AppModel {
    func fetchInbox() async {
        guard let rustClient else { return }
        inboxEntries = await rustClient.fetchInbox()
    }

    func setMood(_ mood: String, text: String? = nil) async {
        guard let rustClient else { return }
        await rustClient.publishMood(mood, text: text)
        currentMood = XMPPUserMood(mood: mood, text: text)
    }

    func clearMood() async {
        guard let rustClient else { return }
        await rustClient.clearMood()
        currentMood = nil
    }

    func setActivity(_ activity: String, text: String? = nil) async {
        guard let rustClient else { return }
        await rustClient.publishActivity(activity, text: text)
        currentActivity = XMPPUserActivity(activity: activity, text: text)
    }

    func setTune(artist: String?, title: String?, source: String? = nil, uri: String? = nil) async {
        guard let rustClient else { return }
        await rustClient.publishTune(artist: artist, title: title, source: source, uri: uri)
        currentTune = XMPPUserTune(artist: artist, title: title, source: source, length: nil, uri: uri)
    }

    func clearTune() async {
        guard let rustClient else { return }
        await rustClient.clearTune()
        currentTune = nil
    }
}
