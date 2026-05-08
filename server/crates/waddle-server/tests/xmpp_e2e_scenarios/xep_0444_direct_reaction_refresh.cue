package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0444-direct-reaction-refresh"
	xeps: ["XEP-0313", "XEP-0334", "XEP-0444"]
	users: {
		alice: devices: phone: #Actor & {
			user:     "alice"
			device:   "phone"
			username: "alice"
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

	let alicePhone = users.alice.devices.phone
	let bobPhone = users.bob.devices.phone

	steps: [
		#SendMessage & {
			from: alicePhone
			to:   bobPhone
			id:   "cue-direct-react-original"
			body: "direct reaction survives refresh"
		},
		#ExpectMessage & {
			target: bobPhone
			from:   alicePhone
			body:   "direct reaction survives refresh"
		},
		#SendMessage & {
			from: bobPhone
			to:   alicePhone
			type: "chat"
			id:   "cue-direct-reaction-1"
			payloads: [
				#Reactions & {
					id:     "cue-direct-react-original"
					emojis: ["🔥"]
				},
				#ProcessingHint & {name: "store"},
			]
		},
		#ExpectMessage & {
			target:     alicePhone
			from:       bobPhone
			bodyAbsent: true
			payloads: [
				#Reactions & {
					id:     "cue-direct-react-original"
					emojis: ["🔥"]
				},
				#ProcessingHint & {name: "store"},
			]
		},
		#DisconnectActor & {actor: alicePhone},
		#ConnectActor & {actor: alicePhone},
		#QueryMam & {
			actor:   alicePhone
			archive: alicePhone.bareJid
			id:      "cue-direct-reaction-refresh-mam"
		},
		#ExpectMamResult & {body: "direct reaction survives refresh"},
		#ExpectMamResult & {
			bodyAbsent: true
			contains: ["cue-direct-reaction-1"]
			payloads: [
				#Reactions & {
					id:     "cue-direct-react-original"
					emojis: ["🔥"]
				},
				#ProcessingHint & {name: "store"},
			]
		},
	]
}
