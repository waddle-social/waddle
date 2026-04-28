package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "dm-delivery-and-mam"
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
			id:   "cue-dm-1"
			body: "hello-from-cue-scenario"
		},
		#ExpectMessage & {
			target: bobPhone
			from:   alicePhone
			body:   "hello-from-cue-scenario"
		},
		#QueryMam & {
			actor:   alicePhone
			archive: alicePhone.bareJid
			id:      "cue-dm-mam"
		},
		#ExpectMamResult & {
			body: "hello-from-cue-scenario"
		},
	]
}
