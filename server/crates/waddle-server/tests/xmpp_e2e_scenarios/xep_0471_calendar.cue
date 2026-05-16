package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0471-calendar"
	xeps: ["XEP-0060", "XEP-0471"]
	users: admin: devices: phone: #Actor & {
		user:     "admin"
		device:   "phone"
		username: "admin"
		resource: "phone"
		domain:   scenario.domain
	}

	let adminPhone = users.admin.devices.phone

	steps: [
		// 1. disco#info on the community service MUST advertise the
		//    XEP-0471 calendar namespace so clients can discover
		//    support before subscribing or publishing.
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-calendar-disco"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-calendar-disco"
			type:   "result"
			contains: [
				"var=\"urn:xmpp:calendar:0\"",
			]
		},
		// 2. Publish an event to the bootstrapped community events
		//    node. Server bootstraps with spaces_public(); admin has
		//    Publisher affiliation via the seed.
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-event-publish"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "publish"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:calendar:0"
						children: [
							#XmlElement & {
								name:  "item"
								ns:    "http://jabber.org/protocol/pubsub"
								attrs: id: "cue-event-1"
								children: [
									#XmlElement & {
										name: "event"
										ns:   "urn:xmpp:calendar:0"
										children: [
											#XmlElement & {
												name: "title"
												ns:   "urn:xmpp:calendar:0"
												text: "Launch day"
											},
											#XmlElement & {
												name: "start"
												ns:   "urn:xmpp:calendar:0"
												text: "2030-06-01T18:00:00Z"
											},
											#XmlElement & {
												name: "organizer"
												ns:   "urn:xmpp:calendar:0"
												text: "admin@\(scenario.domain)"
											},
										]
									},
								]
							},
						]
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-event-publish"
			type:   "result"
		},
		// 3. Items query MUST return the published event with the
		//    typed <event/> payload intact (id, title, start,
		//    organizer).
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-event-items"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "items"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:calendar:0"
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-event-items"
			type:   "result"
			contains: [
				"id=\"cue-event-1\"",
				"urn:xmpp:calendar:0",
				"Launch day",
				"2030-06-01T18:00:00Z",
			]
		},
	]
}
