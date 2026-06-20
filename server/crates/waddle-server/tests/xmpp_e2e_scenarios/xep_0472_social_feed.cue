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
				"var='urn:xmpp:pubsub-social-feed:1'",
			]
		},
		// 2. Publish a feed entry to the bootstrapped community feed
		//    node. The server bootstraps this node at startup with
		//    community_feed() config (open read + open publish), so any
		//    authenticated member may post; admin publishes here.
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
						attrs: node: "urn:xmpp:pubsub-social-feed:1"
						children: [
							#XmlElement & {
								name:  "item"
								ns:    "http://jabber.org/protocol/pubsub"
								attrs: id: "cue-post-1"
								children: [
									#XmlElement & {
										name: "entry"
										ns:   "http://www.w3.org/2005/Atom"
										children: [
											#XmlElement & {
												name:  "title"
												ns:    "http://www.w3.org/2005/Atom"
												attrs: type: "text"
												text: "First post"
											},
											#XmlElement & {
												name: "id"
												ns:   "http://www.w3.org/2005/Atom"
												text: "tag:localhost,2026:cue-post-1"
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
												text: "Hello community feed!"
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
		//    typed Atom <entry/> payload intact (id, title, content,
		//    author).
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
						attrs: node: "urn:xmpp:pubsub-social-feed:1"
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-feed-items"
			type:   "result"
			contains: [
				"id='cue-post-1'",
				"urn:xmpp:pubsub-social-feed:1",
				"http://www.w3.org/2005/Atom",
				"First post",
				"Hello community feed!",
			]
		},
	]
}
