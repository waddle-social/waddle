package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0511-client-authored-link-metadata-is-stripped"
	description: "Client-authored XEP-0511 metadata is not trusted; the server only stamps metadata from signed Waddle preview requests."
	xeps: ["XEP-0313", "XEP-0511"]
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
			absent: [canonical, "The Best Webpage", "https://ogp.me/ns#"]
			absentElements: [
				#XmlElement & {
					name: "Description"
					ns:   "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
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
			absent: [canonical, "The Best Webpage", "https://ogp.me/ns#"]
			absentElements: [
				#XmlElement & {
					name: "Description"
					ns:   "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
				},
			]
		},
	]
}
