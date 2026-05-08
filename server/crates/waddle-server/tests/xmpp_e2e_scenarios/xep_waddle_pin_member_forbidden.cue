package xmpp_e2e_scenarios

// Asserts that a non-admin member who tries to pin a message receives
// a forbidden error and that no pin-event broadcast is emitted (#414,
// member-pin-rejection path of the MucPinHandler).

scenario: #Scenario & {
	name: "xep-waddle-pin-member-forbidden"
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

	let roomJid = "cue-pin-forbidden@muc.\(scenario.domain)"
	let alicePhone = users.alice.devices.phone
	let bobPhone = users.bob.devices.phone

	steps: [
		// Alice (owner) and Bob (member) join.
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice"},
		#SetMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "member"
			id:          "cue-pin-fb-set-bob-member"
		},
		#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob"},

		// Alice posts a message; Bob captures its stanza-id so he can
		// attempt to pin it.
		#SendMessage & {
			from:  alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-pin-fb-target"
			body:  "message bob will try to pin"
		},
		#ExpectMessage & {
			target:            bobPhone
			body:              "message bob will try to pin"
			contains:          [roomJid, "cue-pin-fb-target"]
			captureStanzaIdAs: "pinTargetForbiddenStanzaId"
			captureStanzaIdBy: roomJid
		},

		// Bob attempts to pin — must be rejected.
		#SendMessage & {
			from:  bobPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-pin-fb-attempt"
			payloads: [
				#PinAttachment & {
					idFrom: "pinTargetForbiddenStanzaId"
					action: "pinned"
				},
			]
		},

		// Bob receives the forbidden error reply.
		#ExpectMessage & {
			target:     bobPhone
			bodyAbsent: true
			contains:   ["forbidden", "type=\"error\""]
		},

		// Alice does NOT receive a pin-event broadcast — the handler
		// halted before reaching the broadcast step.
		#ExpectNoStanza & {
			target:   alicePhone
			contains: ["pin-event"]
			millis:   500
		},

		#DrainFrames & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#DrainFrames & {
			target:   alicePhone
			contains: ["<presence", "from=\"\(roomJid)/bob\""]
		},
		#DrainFrames & {
			target:   alicePhone
			contains: ["id=\"cue-pin-fb-target\"", "message bob will try to pin"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["<presence", "from=\"\(roomJid)/alice\""]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["urn:waddle:inbox:0", "cue-pin-fb-target"]
		},
	]
}
