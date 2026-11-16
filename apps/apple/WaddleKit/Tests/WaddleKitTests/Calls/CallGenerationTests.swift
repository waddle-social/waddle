import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite struct CallGenerationTests {
    @Test(arguments: ["propose", "proceed", "reject", "finish"])
    func clearFencesSuspendedLocalActions(verb: String) async {
        let f = CallFixture()
        if verb != "propose" {
            f.store.handle(f.event(.propose(callAudio)))
            await eventually { f.port.callSends.count == 1 }
            if verb == "finish" { #expect(await f.store.acceptIncoming()) }
        }
        f.port.callSends = []
        f.port.heldCallVerbs = [verb]
        f.port.callResults[verb] = false
        let action = Task {
            switch verb {
            case "propose": return await f.store.startCall(to: callPeer.bare, media: callAudio)
            case "proceed": return await f.store.acceptIncoming()
            default: return await f.store.declineIncoming()
            }
        }
        await eventually { f.port.callSends.count == 1 }
        f.store.clear()
        f.port.heldCallVerbs = []
        f.port.callResults = [:]
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        f.port.releaseCallSends()
        #expect(await !action.value)
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callVideo, initiator: callOwn)))
        #expect(f.store.lastError == nil)
        #expect(f.port.callSends.count == 2)
        f.store.clear()
    }
    @Test(arguments: [CallTerminateOutcome.ok, .orphaned, .error])
    func clearFencesLateTerminateAndFinish(outcome: CallTerminateOutcome) async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        f.port.callSends = []
        f.port.heldCallVerbs = ["terminate"]
        f.port.callTerminateOutcome = outcome
        let teardown = Task { await f.store.hangUp() }
        await eventually { f.port.callSends.count == 1 }
        f.store.clear()
        f.port.heldCallVerbs = []
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        f.port.releaseCallSends()
        await teardown.value
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callVideo, initiator: callOwn)))
        #expect(f.store.lastError == nil)
        #expect(f.port.callSends == [.terminate(callPeer, "c1", .success), .propose(callPeer.bare, "c1", callVideo)])
        f.store.clear()
    }

    @Test(arguments: ["ringing", "initiate", "accept", "reject", "retractTieBreak", "migrated", "proceed"], [true, false])
    func clearFencesRemoteSendsAndTheirFollowUps(verb: String, result: Bool) async {
        let f = CallFixture(sid: { "c-zz" })
        if verb == "initiate" || verb == "retractTieBreak" || verb == "migrated" || verb == "proceed" {
            #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
            if verb == "migrated" || verb == "proceed" {
                f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c-zz"))
            }
        } else if verb == "accept" || verb == "reject" {
            f.store.handle(f.event(.propose(callAudio)))
            await eventually { f.port.callSends.count == 1 }
        }
        f.port.callSends = []
        f.port.heldCallVerbs = [verb]
        f.port.callResults[verb] = result
        switch verb {
        case "initiate": f.store.handle(f.event(.proceed, sid: "c-zz"))
        case "accept": f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio)))
        case "reject": f.store.handle(f.event(.propose(callAudio), sid: "c-other", from: JID(parsing: "carol@waddle.test/tab")!))
        default: f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        }
        await eventually { f.port.callSends.last?.verb == verb }
        #expect(f.port.callSends.last?.verb == verb)
        let sendsBeforeClear: [CallSend]
        switch verb {
        case "ringing": sendsBeforeClear = [.ringing(callPeer.bare, "c-aa")]
        case "initiate": sendsBeforeClear = [.initiate(callPeer, callOwn, "c-zz", callAudio)]
        case "accept": sendsBeforeClear = [.accept(callPeer, callOwn, "c1", callAudio)]
        case "reject": sendsBeforeClear = [.reject(JID(parsing: "carol@waddle.test/tab")!, "c-other", false)]
        case "retractTieBreak": sendsBeforeClear = [.retractTieBreak(callPeer, "c-zz")]
        case "migrated": sendsBeforeClear = [.migrated(callPeer, "c-zz", "c-aa")]
        default: sendsBeforeClear = [.migrated(callPeer, "c-zz", "c-aa"), .proceed(callPeer, "c-aa")]
        }
        #expect(f.port.callSends == sendsBeforeClear)
        f.store.clear()
        f.port.callResults = [:]
        f.port.heldCallVerbs = []
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        f.port.releaseCallSends()
        await eventually { f.port.completedCallSends.contains(sendsBeforeClear.last!) }
        // This public busy response also checks the new generation's effect drain.
        let carol = JID(parsing: "carol@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio), sid: "c-barrier", from: carol))
        await eventually { f.port.callSends.last == .reject(carol, "c-barrier", false) }
        #expect(f.port.callSends == sendsBeforeClear + [.propose(callPeer.bare, "c-zz", callVideo), .reject(carol, "c-barrier", false)])
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c-zz", media: callVideo, initiator: callOwn)))
        #expect(f.store.lastError == nil)
        f.store.clear()
    }

    @Test(arguments: ["retract", "finish", "terminate"])
    func clearFencesSuspendedTimeoutSends(verb: String) async {
        let f = CallFixture(ringTimeout: .milliseconds(30), sessionTimeout: .milliseconds(30))
        f.port.heldCallVerbs = [verb]
        f.port.callResults[verb] = false
        let expected: [CallSend]
        if verb == "finish" {
            f.store.handle(f.event(.propose(callAudio)))
            await eventually { f.port.callSends.count == 1 }
            #expect(await f.store.acceptIncoming())
            expected = [.ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1"), .finish(callPeer, "c1", .timeout)]
        } else {
            #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
            if verb == "terminate" {
                f.store.handle(f.event(.proceed))
                expected = [
                    .propose(callPeer.bare, "c1", callAudio), .initiate(callPeer, callOwn, "c1", callAudio),
                    .terminate(callPeer, "c1", .timeout),
                ]
            } else {
                expected = [.propose(callPeer.bare, "c1", callAudio), .retract(callPeer.bare, "c1")]
            }
        }
        await eventually { f.port.callSends.last?.verb == verb }
        #expect(f.port.callSends == expected)
        #expect(f.store.state == .ended(sid: "c1", reason: .timeout))
        f.store.clear()
        f.port.heldCallVerbs = []
        f.port.callResults = [:]
        f.store.handle(f.event(.propose(callVideo), sid: "c-live"))
        f.port.releaseCallSends()
        await eventually { f.port.completedCallSends.contains(expected.last!) }
        let carol = JID(parsing: "carol@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio), sid: "c-barrier", from: carol))
        await eventually { f.port.callSends.last == .reject(carol, "c-barrier", false) }
        #expect(f.port.callSends == expected + [.ringing(callPeer.bare, "c-live"), .reject(carol, "c-barrier", false)])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c-live", media: callVideo)))
        #expect(f.store.lastError == nil)
        f.store.clear()
    }

}
