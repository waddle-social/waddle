package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "multi-device-carbons"
	users: {
		alice: devices: {
			phone: #Actor & {
				user:     "alice"
				device:   "phone"
				username: "alice"
				resource: "phone"
				domain:   scenario.domain
			}
			desktop: #Actor & {
				user:     "alice"
				device:   "desktop"
				username: "alice"
				resource: "desktop"
				domain:   scenario.domain
			}
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
	let aliceDesktop = users.alice.devices.desktop
	let bobPhone = users.bob.devices.phone

	steps: [
		#EnableCarbons & {actor: aliceDesktop},
		#SendMessage & {
			from: alicePhone
			to:   bobPhone
			id:   "cue-carbon-sent"
			body: "sent-carbon-proof"
		},
		#ExpectMessage & {
			target: bobPhone
			from:   alicePhone
			body:   "sent-carbon-proof"
		},
		#ExpectCarbon & {
			target: aliceDesktop
			carbon: "sent"
			body:   "sent-carbon-proof"
		},
		#SendMessage & {
			from: bobPhone
			to:   alicePhone
			id:   "cue-carbon-received"
			body: "received-carbon-proof"
		},
		#ExpectMessage & {
			target: alicePhone
			from:   bobPhone
			body:   "received-carbon-proof"
		},
		#ExpectCarbon & {
			target: aliceDesktop
			carbon: "received"
			body:   "received-carbon-proof"
		},
		#QueryMam & {
			actor:   alicePhone
			archive: alicePhone.bareJid
			id:      "cue-carbon-mam"
		},
		#ExpectMamResult & {body: "sent-carbon-proof"},
		#ExpectMamResult & {body: "received-carbon-proof"},
	]
}
