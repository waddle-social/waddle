#if os(macOS)
import CoreAudio
import Darwin
import Foundation

/// LiveKit reports an audio-engine failure without the underlying device
/// error. Core Audio can confirm whether another process has exclusive use.
enum CallMicrophoneUse {
    static func isHeldByAnotherProcess(_ preferredID: String?) -> Bool {
        let device = inputDevice(preferredID)
        guard device != kAudioObjectUnknown else { return false }
        var address = property(kAudioDevicePropertyHogMode)
        var owner: pid_t = -1
        var size = UInt32(MemoryLayout<pid_t>.size)
        let status = AudioObjectGetPropertyData(device, &address, 0, nil, &size, &owner)
        return status == noErr && owner > 0 && owner != getpid()
    }

    private static func inputDevice(_ preferredID: String?) -> AudioDeviceID {
        let system = AudioObjectID(kAudioObjectSystemObject)
        var device = AudioDeviceID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        let status: OSStatus
        if let preferredID {
            var address = property(kAudioHardwarePropertyTranslateUIDToDevice)
            var uid = preferredID as CFString
            status = withUnsafePointer(to: &uid) { uid in
                AudioObjectGetPropertyData(system, &address, UInt32(MemoryLayout<CFString>.size), uid, &size, &device)
            }
        } else {
            var address = property(kAudioHardwarePropertyDefaultInputDevice)
            status = AudioObjectGetPropertyData(system, &address, 0, nil, &size, &device)
        }
        return status == noErr ? device : AudioDeviceID(kAudioObjectUnknown)
    }

    private static func property(_ selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress(
            mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain
        )
    }
}
#endif
