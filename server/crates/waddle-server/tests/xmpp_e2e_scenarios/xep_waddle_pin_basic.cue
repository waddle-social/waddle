package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-waddle-pin-basic"
	users: {
		alice: devices: phone: #Actor & {
			user:     "alice"
			device:   "phone"
			username: "admin"
			resource: "phone"
			domain:   scenario.domain
		}
		bob: devices: phone: #Actor & {
			user:     "bob"
			device:   "phone"
			username: "bob"
			resource: "phone"
			domain:   scenario.domain
		}
	}

	let roomJid = "cue-pin@muc.\(scenario.domain)"
	let alicePhone = users.alice.devices.phone
	let bobPhone = users.bob.devices.phone

	steps: [
		// Alice (room owner) and Bob (member) both join.
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice"},
		#SetMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "member"
			id:          "cue-pin-set-bob-member"
		},
		#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob"},

		// Bob posts an important message; capture its room stanza-id.
		#SendMessage & {
			from:  bobPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-pin-target"
			body:  "important message that will be pinned"
		},
		#ExpectMessage & {
			target:            alicePhone
			body:              "important message that will be pinned"
			contains:          [roomJid, "cue-pin-target"]
			captureStanzaIdAs: "pinTargetStanzaId"
			captureStanzaIdBy: roomJid
		},

		// Alice (owner) pins Bob's message via XEP-0470 attachment.
		#SendMessage & {
			from:  alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-pin-pin-1"
			payloads: [
				#PinAttachment & {
					idFrom: "pinTargetStanzaId"
					action: "pinned"
				},
			]
		},

		// Both members observe the pin-event system message broadcast
		// from the room bare JID, archived in MAM.
		#ExpectMessage & {
			target:   alicePhone
			body:     "alice pinned a message"
			contains: [roomJid]
			payloads: [
				#PinEvent & {
					idFrom: "pinTargetStanzaId"
					action: "pinned"
				},
			]
		},
		#ExpectMessage & {
			target:   bobPhone
			body:     "alice pinned a message"
			contains: [roomJid]
			payloads: [
				#PinEvent & {
					idFrom: "pinTargetStanzaId"
					action: "pinned"
				},
			]
		},

		// Alice unpins.
		#SendMessage & {
			from:  alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-pin-unpin-1"
			payloads: [
				#PinAttachment & {
					idFrom: "pinTargetStanzaId"
					action: "unpinned"
				},
			]
		},
		#ExpectMessage & {
			target:   alicePhone
			body:     "alice unpinned a message"
			contains: [roomJid]
			payloads: [
				#PinEvent & {
					idFrom: "pinTargetStanzaId"
					action: "unpinned"
				},
			]
		},
		#ExpectMessage & {
			target:   bobPhone
			body:     "alice unpinned a message"
			contains: [roomJid]
			payloads: [
				#PinEvent & {
					idFrom: "pinTargetStanzaId"
					action: "unpinned"
				},
			]
		},

		#DrainFrames & {
			target:   alicePhone
			contains: ["from='\(roomJid)'", "<subject></subject>"]
		},
		#DrainFrames & {
			target:   alicePhone
			contains: ["<presence", "from='\(roomJid)/bob'"]
		},
		#DrainFrames & {
			target:   alicePhone
			contains: ["urn:xmpp:inbox:0", "cue-pin-target"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["<presence", "from='\(roomJid)/alice'"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["from='\(roomJid)'", "<subject></subject>"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["id='cue-pin-target'", "important message that will be pinned"]
		},
	]
}
