package xmpp_e2e_suite

scenario: {
	name: "first-proof-dm-delivery-and-mam"
	domain: "localhost"
	devices: {
		alice_phone: #Device & {
			id:       "alice_phone"
			username: "alice"
			resource: "phone"
		}
		bob_phone: #Device & {
			id:       "bob_phone"
			username: "bob"
			resource: "phone"
		}
	}
	users: [
		#User & {
			id: "alice"
			devices: [scenario.devices.alice_phone]
		},
		#User & {
			id: "bob"
			devices: [scenario.devices.bob_phone]
		},
	]

	steps: [
		#SendMessage & {
			actor: scenario.devices.alice_phone.id
			message: #Message & {
				to:   "\(scenario.devices.bob_phone.username)@\(domain)/\(scenario.devices.bob_phone.resource)"
				id:   "proof-1"
				body: "hello-from-cue-scenario"
			}
		},
		#ExpectContains & {
			target: scenario.devices.bob_phone.id
			contains: [
				"<body>hello-from-cue-scenario</body>",
				"\(scenario.devices.alice_phone.username)@\(domain)",
			]
		},
		#ExpectMamRows & {
			body: "hello-from-cue-scenario"
		},
	]
}
