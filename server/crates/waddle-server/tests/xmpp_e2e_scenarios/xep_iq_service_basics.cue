package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-iq-service-basics"
	xeps: [
		"XEP-0004",
		"XEP-0012",
		"XEP-0030",
		"XEP-0050",
		"XEP-0055",
		"XEP-0092",
		"XEP-0198",
		"XEP-0199",
		"XEP-0202",
		"XEP-0237",
		"XEP-0292",
		"XEP-0357",
		"XEP-0363",
		"XEP-0433",
	]
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
			id:    "cue-disco-server"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-disco-server"
			type:   "result"
			contains: [
				"http://jabber.org/protocol/disco#info",
				"http://jabber.org/protocol/disco#items",
				"http://jabber.org/protocol/caps",
				"urn:xmpp:features:rosterver",
				"urn:xmpp:sid:0",
				"urn:xmpp:sm:3",
				"jabber:iq:roster",
				"urn:xmpp:carbons:2",
				"urn:xmpp:carbons:rules:0",
				"urn:xmpp:receipts",
				"msgoffline",
				"vcard-temp",
				"urn:ietf:params:xml:ns:vcard-4.0",
				"urn:xmpp:http:upload:0",
				"jabber:iq:last",
				"urn:xmpp:blocking",
				"urn:xmpp:ping",
				"urn:xmpp:time",
				"jabber:iq:version",
				"jabber:iq:private",
				"http://jabber.org/protocol/commands",
				"jabber:iq:search",
			]
			absent: [
				"http://jabber.org/protocol/bytestreams",
				"urn:xmpp:csi:0",
				"urn:xmpp:mam:2",
				"http://jabber.org/protocol/pubsub#pep",
				"urn:xmpp:avatar:metadata+notify",
				"urn:xmpp:pep-vcard-conversion:0",
				"urn:xmpp:serverinfo:0",
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-disco-commands"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
				attrs: node: "http://jabber.org/protocol/commands"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-disco-commands"
			type:   "result"
			contains: ["http://jabber.org/protocol/commands"]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-mam-form"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "query"
				ns:   "urn:xmpp:mam:2"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-mam-form"
			type:   "result"
			contains: [
				"jabber:x:data",
				"FORM_TYPE",
				"urn:xmpp:mam:2",
				"var='ids'",
				"http://jabber.org/protocol/xdata-validate",
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-last"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "query"
				ns:   "jabber:iq:last"
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-last"
			type:     "result"
			elements: [#XmlElement & {
				name:         "query"
				ns:           "jabber:iq:last"
				attrsPresent: ["seconds"]
			}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-time"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "time"
				ns:   "urn:xmpp:time"
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-time"
			type:     "result"
			elements: [
				#XmlElement & {name: "utc", ns: "urn:xmpp:time"},
				#XmlElement & {name: "tzo", ns: "urn:xmpp:time"},
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-version"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "query"
				ns:   "jabber:iq:version"
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-version"
			type:     "result"
			elements: [#XmlElement & {name: "name", ns: "jabber:iq:version", text: "Waddle"}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-ping"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "ping"
				ns:   "urn:xmpp:ping"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-ping"
			type:   "result"
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-roster-ver"
			payload: #XmlElement & {
				name: "query"
				ns:   "jabber:iq:roster"
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-roster-ver"
			type:     "result"
			elements: [#XmlElement & {
				name:         "query"
				ns:           "jabber:iq:roster"
				attrsPresent: ["ver"]
			}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-user-search-form"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "query"
				ns:   "jabber:iq:search"
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-user-search-form"
			type:     "result"
			elements: [
				#XmlElement & {name: "instructions", ns: "jabber:iq:search"},
				#XmlElement & {name: "nick", ns: "jabber:iq:search"},
			]
			absentElements: [#XmlElement & {name: "email", ns: "jabber:iq:search"}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-user-search"
			to:    scenario.domain
			payload: #XmlElement & {
				name: "query"
				ns:   "jabber:iq:search"
				children: [
					#XmlElement & {name: "nick", ns: "jabber:iq:search", text: "admin"},
				]
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-user-search"
			type:     "result"
			contains: ["admin@localhost"]
			absentElements: [#XmlElement & {name: "email", ns: "jabber:iq:search"}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-upload-disco"
			to:    "upload.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-upload-disco"
			type:     "result"
			contains: ["urn:xmpp:http:upload:0"]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-push-service-disco"
			to:    "push.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-push-service-disco"
			type:   "result"
			contains: [
				"http://jabber.org/protocol/pubsub#access-whitelist",
				"http://jabber.org/protocol/pubsub#publish-only-affiliation",
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-upload-slot"
			to:    "upload.\(scenario.domain)"
			payload: #XmlElement & {
				name: "request"
				ns:   "urn:xmpp:http:upload:0"
				attrs: {
					filename:       "cue.txt"
					size:           "12"
					"content-type": "text/plain"
				}
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-upload-slot"
			type:     "result"
			contains: ["cue.txt"]
			elements: [
				#XmlElement & {name: "slot", ns: "urn:xmpp:http:upload:0"},
				#XmlElement & {name: "put", ns: "urn:xmpp:http:upload:0"},
				#XmlElement & {name: "get", ns: "urn:xmpp:http:upload:0"},
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-push-enable"
			payload: #XmlElement & {
				name: "enable"
				ns:   "urn:xmpp:push:0"
				attrs: {
					jid:  "push-provider.\(scenario.domain)"
					node: "web"
				}
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-push-enable", type: "error", contains: ["service-unavailable"]},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-push-disable"
			payload: #XmlElement & {
				name: "disable"
				ns:   "urn:xmpp:push:0"
				attrs: {
					jid:  "push-provider.\(scenario.domain)"
					node: "web"
				}
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-push-disable", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-channel-search"
			to:    "muc.\(scenario.domain)"
			payload: #XmlElement & {
				name: "search"
				ns:   "urn:xmpp:channel-search:0"
				children: [
					#XmlElement & {name: "query", ns: "urn:xmpp:channel-search:0"},
					#XmlElement & {name: "max", ns: "urn:xmpp:channel-search:0", text: "5"},
				]
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-channel-search"
			type:     "result"
			contains: ["urn:xmpp:channel-search:0"]
		},
		#StreamManagement & {
			actor:  adminPhone
			action: "enable"
			resume: true
			max:    60
		},
		#ExpectFrame & {
			target:   adminPhone
			contains: ["urn:xmpp:sm:3"]
			elements: [#XmlElement & {name: "enabled", ns: "urn:xmpp:sm:3"}]
		},
		#StreamManagement & {
			actor:  adminPhone
			action: "requestAck"
		},
		#ExpectFrame & {
			target:   adminPhone
			contains: ["urn:xmpp:sm:3"]
			elements: [#XmlElement & {
				name:         "a"
				ns:           "urn:xmpp:sm:3"
				attrsPresent: ["h"]
			}]
		},
	]
}
