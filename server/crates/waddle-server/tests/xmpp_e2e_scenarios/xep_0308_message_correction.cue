package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0308-message-correction"
	xeps: ["XEP-0308", "XEP-0313"]
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
			id:   "cue-edit-original"
			body: "helo from cue"
		},
		#ExpectMessage & {
			target: bobPhone
			from:   alicePhone
			body:   "helo from cue"
		},
		#SendMessage & {
			from: alicePhone
			to:   bobPhone
			id:   "cue-edit-correction"
			body: "hello from cue"
			payloads: [
				#MessageCorrection & {id: "cue-edit-original"},
			]
		},
		#ExpectMessage & {
			target: bobPhone
			from:   alicePhone
			body:   "hello from cue"
			payloads: [
				#MessageCorrection & {id: "cue-edit-original"},
			]
		},
		#QueryMam & {
			actor:   alicePhone
			archive: alicePhone.bareJid
			id:      "cue-edit-mam"
		},
		#ExpectMamResult & {body: "helo from cue"},
		#ExpectMamResult & {
			body: "hello from cue"
			payloads: [
				#MessageCorrection & {id: "cue-edit-original"},
			]
		},
	]
}
