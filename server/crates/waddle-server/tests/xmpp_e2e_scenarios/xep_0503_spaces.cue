package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0503-spaces"
	xeps: ["XEP-0060", "XEP-0402", "XEP-0503"]
	users: admin: devices: phone: #Actor & {
		user:     "admin"
		device:   "phone"
		username: "admin"
		resource: "phone"
		domain:   scenario.domain
	}

	let adminPhone = users.admin.devices.phone

	steps: [
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-spaces-items"
			to:    "spaces.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#items"
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-spaces-items"
			type:     "result"
			contains: ["node='general'"]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-spaces-disco"
			to:    "spaces.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-spaces-disco"
			type:   "result"
			contains: [
				"http://jabber.org/protocol/pubsub#subscribe",
				"http://jabber.org/protocol/pubsub#create-nodes",
				"http://jabber.org/protocol/pubsub#config-node",
				"http://jabber.org/protocol/pubsub#meta-data",
				"http://jabber.org/protocol/pubsub#delete-nodes",
				"http://jabber.org/protocol/pubsub#delete-items",
				"http://jabber.org/protocol/pubsub#retract-items",
				"http://jabber.org/protocol/pubsub#item-ids",
				"http://jabber.org/protocol/pubsub#multi-items",
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-general-space-items"
			to:    "spaces.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "items"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "general"
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-general-space-items"
			type:   "result"
			contains: [
				"chat@muc.localhost",
				"announcements@muc.localhost",
				"conference",
				"urn:xmpp:bookmarks:1",
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-space-room-info"
			to:    "chat@muc.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-space-room-info"
			type:   "result"
			contains: [
				"urn:xmpp:spaces:0",
				"var='parent'",
				"xmpp:spaces.localhost?;node=general",
				"http://jabber.org/protocol/muc#roominfo",
				"muc#roomconfig_pubsub",
			]
		},
	]
}
