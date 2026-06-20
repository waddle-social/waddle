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
		// 1. disco#info on the community service MUST advertise the
		//    XEP-0501 stories namespace so clients can discover
		//    support before subscribing or publishing.
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-stories-disco"
			to:    "community.\(scenario.domain)"
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
				"var='urn:xmpp:pubsub-social-feed:stories:0'",
			]
		},
		// 2. Publish a story to the bootstrapped community stories
		//    node. The server bootstraps this node at startup with
		//    the XEP-0501 story profile; server-owner affiliation
		//    seed grants admin Publisher access.
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-story-publish"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "publish"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:pubsub-social-feed:stories:0"
						children: [
							#XmlElement & {
								name:  "item"
								ns:    "http://jabber.org/protocol/pubsub"
								attrs: id: "cue-story-1"
								children: [
									#XmlElement & {
										name: "entry"
										ns:   "http://www.w3.org/2005/Atom"
										children: [
											#XmlElement & {
												name:  "title"
												ns:    "http://www.w3.org/2005/Atom"
												attrs: type: "text"
												text: "Look at this!"
											},
											#XmlElement & {
												name: "id"
												ns:   "http://www.w3.org/2005/Atom"
												text: "cue-story-1"
											},
											#XmlElement & {
												name: "published"
												ns:   "http://www.w3.org/2005/Atom"
												text: "2026-06-01T12:00:00Z"
											},
											#XmlElement & {
												name: "updated"
												ns:   "http://www.w3.org/2005/Atom"
												text: "2026-06-01T12:00:00Z"
											},
											#XmlElement & {
												name:  "content"
												ns:    "http://www.w3.org/2005/Atom"
												attrs: type: "text"
												text: "Look at this!"
											},
											#XmlElement & {
												name: "author"
												ns:   "http://www.w3.org/2005/Atom"
												children: [
													#XmlElement & {
														name: "uri"
														ns:   "http://www.w3.org/2005/Atom"
														text: "xmpp:admin@\(scenario.domain)"
													},
												]
											},
											#XmlElement & {
												name: "link"
												ns:   "http://www.w3.org/2005/Atom"
												attrs: {
													rel:  "enclosure"
													href: "https://example.com/photo.jpg"
													type: "image/jpeg"
												}
											},
											#XmlElement & {
												name: "expires"
												ns:   "urn:waddle:stories:0"
												text: "2030-01-01T12:00:00Z"
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
		//    typed Atom payload intact (id, body, enclosure, expires).
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-story-items"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "items"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:pubsub-social-feed:stories:0"
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-story-items"
			type:   "result"
			contains: [
				"id='cue-story-1'",
				"urn:xmpp:pubsub-social-feed:stories:0",
				"http://www.w3.org/2005/Atom",
				"Look at this!",
				"https://example.com/photo.jpg",
				"2030-01-01T12:00:00Z",
			]
		},
	]
}
