package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0472-social-feed"
	xeps: ["XEP-0060", "XEP-0472"]
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
		//    XEP-0472 social feed namespace so clients can discover
		//    support before subscribing or publishing.
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-feed-disco"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-feed-disco"
			type:   "result"
			contains: [
				"var=\"urn:xmpp:pubsub-social-feed:0\"",
			]
		},
		// 2. Publish a feed entry to the bootstrapped community feed
		//    node. The server bootstraps this node at startup with
		//    spaces_public() access; server-owner affiliation seed
		//    grants admin Publisher access.
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-feed-publish"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "publish"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:pubsub-social-feed:0"
						children: [
							#XmlElement & {
								name:  "item"
								ns:    "http://jabber.org/protocol/pubsub"
								attrs: id: "cue-post-1"
								children: [
									#XmlElement & {
										name: "entry"
										ns:   "urn:xmpp:pubsub-social-feed:0"
										children: [
											#XmlElement & {
												name: "title"
												ns:   "urn:xmpp:pubsub-social-feed:0"
												text: "First post"
											},
											#XmlElement & {
												name: "body"
												ns:   "urn:xmpp:pubsub-social-feed:0"
												text: "Hello community feed!"
											},
											#XmlElement & {
												name: "author"
												ns:   "urn:xmpp:pubsub-social-feed:0"
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
			id:     "cue-feed-publish"
			type:   "result"
		},
		// 3. Items query MUST return the published entry with the
		//    typed <entry/> payload intact (id, title, body, author).
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-feed-items"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "items"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:pubsub-social-feed:0"
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-feed-items"
			type:   "result"
			contains: [
				"id=\"cue-post-1\"",
				"urn:xmpp:pubsub-social-feed:0",
				"First post",
				"Hello community feed!",
			]
		},
	]
}
