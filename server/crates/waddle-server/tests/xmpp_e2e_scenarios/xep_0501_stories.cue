package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0501-stories"
	xeps: ["XEP-0060", "XEP-0501"]
	users: admin: devices: phone: #Actor & {
		user:     "admin"
		device:   "phone"
		username: "admin"
		resource: "phone"
		domain:   scenario.domain
	}

	let adminPhone = users.admin.devices.phone

	steps: [
		// 1. disco#info on the spaces service MUST advertise the
		//    XEP-0501 stories namespace so clients can discover
		//    support before subscribing or publishing.
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-stories-disco"
			to:    "spaces.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-stories-disco"
			type:   "result"
			contains: [
				"var=\"urn:xmpp:stories:0\"",
			]
		},
		// 2. Publish a story to the bootstrapped community stories
		//    node. The server bootstraps this node at startup with
		//    spaces_public() access; server-owner affiliation seed
		//    grants admin Publisher access.
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-story-publish"
			to:    "spaces.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "publish"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:stories:0"
						children: [
							#XmlElement & {
								name:  "item"
								ns:    "http://jabber.org/protocol/pubsub"
								attrs: id: "cue-story-1"
								children: [
									#XmlElement & {
										name: "story"
										ns:   "urn:xmpp:stories:0"
										attrs: expires: "2030-01-01T12:00:00Z"
										children: [
											#XmlElement & {
												name: "body"
												ns:   "urn:xmpp:stories:0"
												text: "Look at this!"
											},
											#XmlElement & {
												name: "media-url"
												ns:   "urn:xmpp:stories:0"
												text: "https://example.com/photo.jpg"
											},
											#XmlElement & {
												name: "author"
												ns:   "urn:xmpp:stories:0"
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
			id:     "cue-story-publish"
			type:   "result"
		},
		// 3. Items query MUST return the published story with the
		//    typed <story/> payload intact (id, body, media-url,
		//    expires).
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-story-items"
			to:    "spaces.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "items"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:stories:0"
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-story-items"
			type:   "result"
			contains: [
				"id=\"cue-story-1\"",
				"urn:xmpp:stories:0",
				"Look at this!",
				"https://example.com/photo.jpg",
				"2030-01-01T12:00:00Z",
			]
		},
	]
}
