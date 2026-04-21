package xmpp_e2e_suite

scenario: {
	name: "multi-device-carbons"
	domain: "localhost"
	users: {
		alice: #User & {
			devices: {
				phone: #Device & {
					username: "alice"
					resource: "phone"
				}
				desktop: #Device & {
					username: "alice"
					resource: "desktop"
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
		// Per-resource carbons opt-in for alice's desktop session.
		{
			send: {
				actor: {
					user:   "alice"
					device: "desktop"
				}
				stanza: "<iq xmlns='jabber:client' type='set' id='carbons-enable-desktop'><enable xmlns='urn:xmpp:carbons:2'/></iq>"
			}
		},
		{
			send: {
				actor: {
					user:   "alice"
					device: "phone"
				}
				stanza: "<message xmlns='jabber:client' type='chat' id='multi-dev-sent-1' to='\(scenario.users.bob.devices.phone.username)@\(domain)/\(scenario.users.bob.devices.phone.resource)'><body>sent-carbon-proof</body></message>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "bob"
				device: "phone"
			}
			contains: [
				"<body>sent-carbon-proof</body>",
				"\(scenario.users.alice.devices.phone.username)@\(domain)",
			]
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "desktop"
			}
			contains: [
				"urn:xmpp:carbons:2",
				"<sent",
				"<body>sent-carbon-proof</body>",
			]
		},
		{
			send: {
				actor: {
					user:   "bob"
					device: "phone"
				}
				stanza: "<message xmlns='jabber:client' type='chat' id='multi-dev-recv-1' to='\(scenario.users.alice.devices.phone.username)@\(domain)/\(scenario.users.alice.devices.phone.resource)'><body>received-carbon-proof</body></message>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "phone"
			}
			contains: [
				"<body>received-carbon-proof</body>",
				"\(scenario.users.bob.devices.phone.username)@\(domain)",
			]
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "desktop"
			}
			contains: [
				"urn:xmpp:carbons:2",
				"<received",
				"<body>received-carbon-proof</body>",
			]
		},
		#ExpectMamRows & {
			body: "sent-carbon-proof"
		},
		#ExpectMamRows & {
			body: "received-carbon-proof"
		},
	]
}
