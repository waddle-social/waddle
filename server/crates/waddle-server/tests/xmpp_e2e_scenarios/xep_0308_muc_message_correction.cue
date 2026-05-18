package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0308-muc-message-correction"
	xeps: ["XEP-0045", "XEP-0308", "XEP-0313"]
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

	let roomJid = "cue-edit@muc.\(scenario.domain)"
	let alicePhone = users.alice.devices.phone
	let bobPhone = users.bob.devices.phone

	steps: [
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice-phone"},
		#ExpectFrame & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob-phone"},
		#ExpectPresence & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)/alice-phone\""]
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#ExpectPresence & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)/bob-phone\""]
		},
		#SendMessage & {
			from: alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-muc-edit-original"
			body:  "helo from muc cue"
		},
		#ExpectMessage & {
			target: alicePhone
			body:   "helo from muc cue"
			contains: [roomJid, "cue-muc-edit-original"]
		},
		#ExpectMessage & {
			target: bobPhone
			body:   "helo from muc cue"
			contains: [roomJid, "cue-muc-edit-original"]
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["urn:xmpp:inbox:0", "cue-muc-edit-original"]
		},
		#SendMessage & {
			from: alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-muc-edit-correction"
			body:  "hello from muc cue"
			payloads: [
				#MessageCorrection & {id: "cue-muc-edit-original"},
			]
		},
		#ExpectMessage & {
			target: alicePhone
			body:   "hello from muc cue"
			payloads: [
				#MessageCorrection & {id: "cue-muc-edit-original"},
			]
		},
		#ExpectMessage & {
			target: bobPhone
			body:   "hello from muc cue"
			payloads: [
				#MessageCorrection & {id: "cue-muc-edit-original"},
			]
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["urn:xmpp:inbox:0", "cue-muc-edit-correction"]
		},
	]
}
