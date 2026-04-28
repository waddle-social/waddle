package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0511-link-metadata"
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
	let linked = "https://the.link.example.com/what-was-linked-to"
	let canonical = "https://example.com/canonical-url/for/what-was-linked-to"
	let linkBody = "sharing a useful link: \(linked)"

	steps: [
		#SendMessage & {
			from: alicePhone
			to:   bobPhone
			id:   "cue-xep-0511"
			body: linkBody
			payloads: [
				#LinkMetadata & {
					about:       linked
					title:       "The Best Webpage"
					description: "This is a great webpage and you will really like it"
					url:         canonical
				},
			]
		},
		#ExpectMessage & {
			target: bobPhone
			body:   linkBody
			payloads: [
				#LinkMetadata & {
					about:       linked
					title:       "The Best Webpage"
					description: "This is a great webpage and you will really like it"
					url:         canonical
				},
			]
		},
		#QueryMam & {
			actor:   alicePhone
			archive: alicePhone.bareJid
			id:      "cue-xep-0511-mam"
		},
		#ExpectMamResult & {
			body: linkBody
			payloads: [
				#LinkMetadata & {
					about:       linked
					title:       "The Best Webpage"
					description: "This is a great webpage and you will really like it"
					url:         canonical
				},
			]
		},
	]
}
