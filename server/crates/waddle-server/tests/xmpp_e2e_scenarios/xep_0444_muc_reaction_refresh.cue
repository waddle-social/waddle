package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0444-muc-reaction-refresh"
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

	let roomJid = "cue-reactions@muc.\(scenario.domain)"
	let alicePhone = users.alice.devices.phone
	let bobPhone = users.bob.devices.phone

	steps: [
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice-phone"},
		#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob-phone"},
		#SendMessage & {
			from:  alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-muc-react-original"
			body:  "muc reaction survives refresh"
		},
		#ExpectMessage & {
			target:             bobPhone
			body:               "muc reaction survives refresh"
			contains:           [roomJid, "cue-muc-react-original"]
			captureStanzaIdAs:  "mucOriginalStanzaId"
			captureStanzaIdBy:  roomJid
		},
		#SendMessage & {
			from:  bobPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-muc-reaction-1"
			payloads: [
				#Reactions & {
					idFrom: "mucOriginalStanzaId"
					emojis: ["👍"]
				},
				#ProcessingHint & {name: "store"},
			]
		},
		#ExpectMessage & {
			target:     alicePhone
			bodyAbsent: true
			contains:   [roomJid, "cue-muc-reaction-1"]
			payloads: [
				#Reactions & {
					idFrom: "mucOriginalStanzaId"
					emojis: ["👍"]
				},
				#ProcessingHint & {name: "store"},
			]
		},
		#DisconnectActor & {actor: alicePhone},
		#ConnectActor & {actor: alicePhone},
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice-phone"},
		#QueryMam & {
			actor:   alicePhone
			archive: roomJid
			id:      "cue-muc-reaction-refresh-mam"
		},
		#ExpectMamResult & {
			bodyAbsent: true
			contains: ["cue-muc-reaction-1"]
			payloads: [
				#Reactions & {
					idFrom: "mucOriginalStanzaId"
					emojis: ["👍"]
				},
				#ProcessingHint & {name: "store"},
			]
		},
	]
}
