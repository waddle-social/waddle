package xmpp_e2e_suite

scenario: {
	name: "dm-delivery-and-mam"
	domain: "localhost"
	users: {
		alice: #User & {
			devices: {
				phone: #Device & {
					username: "alice"
					resource: "phone"
				}
			}
		}
		bob: #User & {
			devices: {
				phone: #Device & {
					username: "bob"
					resource: "phone"
				}
			}
		}
	}

	steps: [
		#SendMessage & {
			actor: {
				user:   "alice"
				device: "phone"
			}
			message: #Message & {
				to:   "\(scenario.users.bob.devices.phone.username)@\(domain)/\(scenario.users.bob.devices.phone.resource)"
				id:   "proof-1"
				body: "hello-from-cue-scenario"
			}
		},
		#ExpectContains & {
			target: {
				user:   "bob"
				device: "phone"
			}
			contains: [
				"<body>hello-from-cue-scenario</body>",
				"\(scenario.users.alice.devices.phone.username)@\(domain)",
			]
		},
		#ExpectMamRows & {
			body: "hello-from-cue-scenario"
		},
	]
}
