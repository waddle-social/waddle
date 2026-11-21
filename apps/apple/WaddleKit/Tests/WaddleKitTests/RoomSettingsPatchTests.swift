import Testing
import WaddleKit

@Suite("Room settings changes")
struct RoomSettingsPatchTests {
    @Test func changedFieldsAreTrimmed() {
        let settings = RoomSettings(name: "Old", summary: "Before", pinPermission: .adminsOnly)
        let patch = RoomSettingsPatch.changes(from: settings, name: "  New\n", summary: " After ", pinPermission: .anyone)
        #expect(patch == RoomSettingsPatch(name: "New", summary: "After", pinPermission: .anyone))
        #expect(!patch.isEmpty)
    }
    @Test func unchangedFieldsSendNothing() {
        let settings = RoomSettings(name: "General", summary: "Chat", pinPermission: .adminsOnly)
        let patch = RoomSettingsPatch.changes(from: settings, name: " General ", summary: " Chat\n", pinPermission: .adminsOnly)
        #expect(patch == RoomSettingsPatch())
        #expect(patch.isEmpty)
    }
    @Test func aRoomWithoutASummaryCanGainOne() {
        let settings = RoomSettings(name: "General", summary: nil, pinPermission: .adminsOnly)
        let patch = RoomSettingsPatch.changes(from: settings, name: "General", summary: "Chat", pinPermission: .adminsOnly)
        #expect(patch == RoomSettingsPatch(summary: "Chat"))
    }
    @Test func aRoomWithoutASummaryKeepsNoneWhenLeftEmpty() {
        let settings = RoomSettings(name: "General", summary: nil, pinPermission: .adminsOnly)
        let patch = RoomSettingsPatch.changes(from: settings, name: "General", summary: " ", pinPermission: .adminsOnly)
        #expect(patch.isEmpty)
    }
    @Test func aPinSettingTheServerDoesNotOfferIsNeverSent() {
        let settings = RoomSettings(name: "General", summary: "Chat", pinPermission: nil)
        let patch = RoomSettingsPatch.changes(from: settings, name: "General", summary: "Chat", pinPermission: .anyone)
        #expect(patch.isEmpty)
    }
    @Test func emptyNameIsOmittedWhileEmptySummaryClears() {
        let settings = RoomSettings(name: "General", summary: "Chat", pinPermission: .anyone)
        let patch = RoomSettingsPatch.changes(from: settings, name: " \n", summary: " \n", pinPermission: nil)
        #expect(patch == RoomSettingsPatch(summary: ""))
        #expect(!patch.isEmpty)
    }
    @Test func emptyNameAloneLeavesNothingToSave() {
        let settings = RoomSettings(name: "General", summary: "Chat", pinPermission: .anyone)
        let patch = RoomSettingsPatch.changes(from: settings, name: " \n", summary: "Chat", pinPermission: nil)
        #expect(patch == RoomSettingsPatch())
        #expect(patch.isEmpty)
    }

    @Test func aSingleChangedFieldDoesNotResendOtherFields() {
        let settings = RoomSettings(name: "General", summary: "Chat", pinPermission: .adminsOnly)
        #expect(RoomSettingsPatch.changes(from: settings, name: "Renamed", summary: "Chat", pinPermission: nil)
                == RoomSettingsPatch(name: "Renamed"))
        #expect(RoomSettingsPatch.changes(from: settings, name: "General", summary: "Updated", pinPermission: nil)
                == RoomSettingsPatch(summary: "Updated"))
        #expect(RoomSettingsPatch.changes(from: settings, name: "General", summary: "Chat", pinPermission: .anyone)
                == RoomSettingsPatch(pinPermission: .anyone))
    }
}
