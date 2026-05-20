package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0444-muc-reaction-refresh"
	xeps: ["XEP-0045", "XEP-0313", "XEP-0334", "XEP-0444"]
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
		#ExpectFrame & {
			target:   alicePhone
			contains: ["from='\(roomJid)'", "<subject></subject>"]
		},
		#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob-phone"},
		#ExpectPresence & {
			target:   bobPhone
			contains: ["from='\(roomJid)/alice-phone'"]
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["from='\(roomJid)'", "<subject></subject>"]
		},
		#ExpectPresence & {
			target:   alicePhone
			contains: ["from='\(roomJid)/bob-phone'"]
		},
		#SendMessage & {
			from:  alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-muc-react-original"
			body:  "muc reaction survives refresh"
		},
		#ExpectMessage & {
			target:   alicePhone
			body:     "muc reaction survives refresh"
			contains: [roomJid, "cue-muc-react-original"]
		},
		#ExpectMessage & {
			target:             bobPhone
			body:               "muc reaction survives refresh"
			contains:           [roomJid, "cue-muc-react-original"]
			captureStanzaIdAs:  "mucOriginalStanzaId"
			captureStanzaIdBy:  roomJid
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["urn:xmpp:inbox:0", "cue-muc-react-original"]
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
			target:     bobPhone
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
		#DrainFrames & {
			target:   alicePhone
			contains: ["urn:xmpp:inbox:0", "cue-muc-reaction-1"]
			millis:   1000
		},
		#DisconnectActor & {actor: alicePhone},
		#DrainFrames & {
			target:   bobPhone
			contains: ["from='\(roomJid)/alice-phone'", "type='unavailable'"]
			millis:   1000
			min:      0
			max:      1
		},
		#ConnectActor & {actor: alicePhone},
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice-phone"},
		#ExpectFrame & {
			target:   alicePhone
			contains: ["from='\(roomJid)'", "<subject></subject>"]
		},
		#ExpectPresence & {
			target:   alicePhone
			contains: ["from='\(roomJid)/bob-phone'"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["from='\(roomJid)/alice-phone'"]
			millis:   1000
		},
		#QueryMam & {
			actor:   alicePhone
			archive: roomJid
			id:      "cue-muc-reaction-refresh-mam"
		},
		#ExpectMamResult & {body: "muc reaction survives refresh"},
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
