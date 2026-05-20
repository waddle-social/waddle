package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-muc-rich-payloads"
	xeps: [
		"XEP-0085",
		"XEP-0115",
		"XEP-0184",
		"XEP-0201",
		"XEP-0203",
		"XEP-0317",
		"XEP-0333",
		"XEP-0359",
		"XEP-0372",
		"XEP-0410",
		"XEP-0421",
		"XEP-0424",
		"XEP-0425",
		"XEP-0431",
		"XEP-0461",
		"XEP-0513",
	]
	users: {
		admin: devices: phone: #Actor & {
			user:     "admin"
			device:   "phone"
			username: "admin"
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

	let adminPhone = users.admin.devices.phone
	let bobPhone = users.bob.devices.phone
	let roomJid = "cue-rich@muc.\(scenario.domain)"
	let originalBody = "rich original body mentions @bob"
	let moderateBody = "moderate target body"

	steps: [
		#SendPresence & {
			actor: adminPhone
			to:    "\(roomJid)/admin"
			payloads: [
				#XmlElement & {
					name: "x"
					ns:   "http://jabber.org/protocol/muc"
					children: [
						#XmlElement & {
							name:  "history"
							ns:    "http://jabber.org/protocol/muc"
							attrs: maxstanzas: "0"
						},
					]
				},
			]
		},
		#ExpectPresence & {
			target: adminPhone
			contains: [
				"status code='110'",
				// XEP-0045 affiliation/role carry authority. XEP-0317
				// hats are descriptive metadata only and MUST NOT be
				// synthesised from owner/admin/moderator.
				"affiliation='owner'",
				"role='moderator'",
			]
		},
		#SendPresence & {
			actor: bobPhone
			to:    "\(roomJid)/bob"
			payloads: [
				#XmlElement & {
					name: "x"
					ns:   "http://jabber.org/protocol/muc"
					children: [
						#XmlElement & {
							name:  "history"
							ns:    "http://jabber.org/protocol/muc"
							attrs: maxstanzas: "0"
						},
					]
				},
			]
		},
		#ExpectPresence & {
			target:   bobPhone
			contains: ["status code='110'"]
		},
		#ExpectPresence & {
			target:   adminPhone
			contains: [roomJid]
			elements: [#XmlElement & {name: "occupant-id", ns: "urn:xmpp:occupant-id:0"}]
			captures: [#AttributeCapture & {
				as:      "bobOccupantId"
				element: "occupant-id"
				ns:      "urn:xmpp:occupant-id:0"
				name:    "id"
			}]
		},
		#SendPresence & {
			actor: adminPhone
			to:    bobPhone.jid
			payloads: [
				#XmlElement & {
					name: "c"
					ns:   "http://jabber.org/protocol/caps"
					attrs: {
						hash: "sha-1"
						node: "https://waddle.social/caps"
						ver:  "cue-caps"
					}
				},
			]
		},
		#ExpectPresence & {
			target:   bobPhone
			contains: ["http://jabber.org/protocol/caps", "cue-caps"]
		},
		#SendMessage & {
			from: adminPhone
			to:   bobPhone
			id:   "cue-chat-state"
			payloads: [
				#XmlPayload & {
					element: #XmlElement & {
						name: "composing"
						ns:   "http://jabber.org/protocol/chatstates"
					}
				},
			]
		},
		#ExpectMessage & {
			target:     bobPhone
			bodyAbsent: true
			elements: [#XmlElement & {name: "composing", ns: "http://jabber.org/protocol/chatstates"}]
		},
		#SendMessage & {
			from: adminPhone
			to:   bobPhone
			id:   "cue-receipt-request"
			body: "please ack this"
			payloads: [
				#XmlPayload & {element: #XmlElement & {name: "request", ns: "urn:xmpp:receipts"}},
				#XmlPayload & {element: #XmlElement & {name: "markable", ns: "urn:xmpp:chat-markers:0"}},
			]
		},
		#ExpectMessage & {
			target:   bobPhone
			body:     "please ack this"
			contains: ["urn:xmpp:receipts", "urn:xmpp:chat-markers:0"]
		},
		#SendMessage & {
			from: bobPhone
			to:   adminPhone
			id:   "cue-receipt-response"
			payloads: [
				#XmlPayload & {
					element: #XmlElement & {
						name:  "received"
						ns:    "urn:xmpp:receipts"
						attrs: id: "cue-receipt-request"
					}
				},
				#XmlPayload & {
					element: #XmlElement & {
						name:  "displayed"
						ns:    "urn:xmpp:chat-markers:0"
						attrs: id: "cue-receipt-request"
					}
				},
			]
		},
		#ExpectMessage & {
			target:     adminPhone
			bodyAbsent: true
			contains:   ["urn:xmpp:receipts", "urn:xmpp:chat-markers:0", "cue-receipt-request"]
		},
		#SendMessage & {
			from: adminPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-rich-original"
			body:  originalBody
			payloads: [
				#XmlPayload & {
					element: #XmlElement & {
						name:  "origin-id"
						ns:    "urn:xmpp:sid:0"
						attrs: id: "cue-origin-rich-1"
					}
				},
				#XmlPayload & {
					element: #XmlElement & {
						name: "stanza-id"
						ns:   "urn:xmpp:sid:0"
						attrs: {
							by: roomJid
							id: "spoofed-room-id"
						}
					}
				},
				#XmlPayload & {
					element: #XmlElement & {
						name: "active"
						ns:   "http://jabber.org/protocol/chatstates"
					}
				},
				#XmlPayload & {
					element: #XmlElement & {
						name: "thread"
						ns:   "jabber:client"
						attrs: parent: "parent-thread"
						text: "child-thread"
					}
				},
				#XmlPayload & {
					element: #XmlElement & {
						name: "reference"
						ns:   "urn:xmpp:reference:0"
						attrs: {
							type:  "mention"
							begin: "28"
							end:   "32"
							uri:   "xmpp:bob@localhost"
						}
					}
				},
				#XmlPayload & {
					element: #XmlElement & {
						name: "mention"
						ns:   "urn:xmpp:mentions:0"
						attrs: {
							begin: "28"
							end:   "32"
						}
						attrsFrom: occupantid: "bobOccupantId"
					}
				},
			]
		},
		#ExpectMessage & {
			target:             adminPhone
			body:               originalBody
			captureStanzaIdAs:  "moderationTarget"
			captureStanzaIdBy:  roomJid
			contains: [
				"urn:xmpp:occupant-id:0",
				"urn:xmpp:sid:0",
				"cue-origin-rich-1",
				"http://jabber.org/protocol/chatstates",
				"urn:xmpp:reference:0",
				"urn:xmpp:mentions:0",
			]
			elements: [
				#XmlElement & {name: "thread", ns: "jabber:client", text: "child-thread"},
				#XmlElement & {
					name:      "mention"
					ns:        "urn:xmpp:mentions:0"
					attrsFrom: occupantid: "bobOccupantId"
				},
			]
			absent: ["spoofed-room-id"]
		},
		#ExpectMessage & {
			target: bobPhone
			body:   originalBody
			contains: [
				"urn:xmpp:occupant-id:0",
				"urn:xmpp:sid:0",
				"cue-origin-rich-1",
				"urn:xmpp:reference:0",
				"urn:xmpp:mentions:0",
			]
			elements: [#XmlElement & {
				name:      "mention"
				ns:        "urn:xmpp:mentions:0"
				attrsFrom: occupantid: "bobOccupantId"
			}]
			absent: ["spoofed-room-id"]
		},
		#SendMessage & {
			from:  bobPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-fulltext-nonmatch"
			body:  "archived room message without the search token"
		},
		#ExpectMessage & {
			target: adminPhone
			body:   "archived room message without the search token"
		},
		#QueryMam & {
			actor:    adminPhone
			archive:  roomJid
			id:       "cue-mam-fulltext"
			fulltext: "@bob"
		},
		#ExpectNoMamResult & {
			body: "archived room message without the search token"
		},
		#ExpectMamResult & {
			body: originalBody
			contains: [
				"urn:xmpp:forward:0",
				"urn:xmpp:delay",
				"urn:xmpp:reference:0",
				"urn:xmpp:mentions:0",
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-muc-self-ping"
			to:    "\(roomJid)/admin"
			payload: #XmlElement & {
				name: "ping"
				ns:   "urn:xmpp:ping"
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-muc-self-ping", type: "result"},
		#SendMessage & {
			from: bobPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-reply"
			body:  "replying to the rich message"
			payloads: [
				#XmlPayload & {
					element: #XmlElement & {
						name: "reply"
						ns:   "urn:xmpp:reply:0"
						attrs: {
							to: "\(roomJid)/admin"
							id: "cue-rich-original"
						}
					}
				},
				#XmlPayload & {
					element: #XmlElement & {
						name:  "fallback"
						ns:    "urn:xmpp:fallback:0"
						attrs: for: "urn:xmpp:reply:0"
						children: [#XmlElement & {name: "body", ns: "urn:xmpp:fallback:0"}]
					}
				},
			]
		},
		#ExpectMessage & {
			target:   adminPhone
			body:     "replying to the rich message"
			contains: ["urn:xmpp:reply:0", "urn:xmpp:fallback:0", "cue-rich-original"]
		},
		#SendMessage & {
			from: adminPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-retract"
			body:  "/me retracted a previous message"
			payloads: [
				#XmlPayload & {
					element: #XmlElement & {
						name:  "retract"
						ns:    "urn:xmpp:message-retract:1"
						attrs: id: "cue-rich-original"
					}
				},
			]
		},
		#ExpectMessage & {
			target:   adminPhone
			body:     "/me retracted a previous message"
			contains: ["urn:xmpp:message-retract:1", "cue-rich-original"]
		},
		#QueryMam & {
			actor:   adminPhone
			archive: roomJid
			id:      "cue-mam-retraction"
			idsFrom: ["moderationTarget"]
		},
		#ExpectNoMamResult & {body: originalBody},
		#ExpectMamResult & {
			bodyAbsent: true
			elements: [#XmlElement & {name: "retracted", ns: "urn:xmpp:message-retract:1"}]
		},
		#SendMessage & {
			from: adminPhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-moderate-original"
			body:  moderateBody
		},
		#ExpectMessage & {
			target:             adminPhone
			body:               moderateBody
			captureStanzaIdAs:  "moderationTarget"
			captureStanzaIdBy:  roomJid
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-moderate"
			to:    roomJid
			payload: #XmlElement & {
				name: "moderate"
				ns:   "urn:xmpp:message-moderate:1"
				attrsFrom: id: "moderationTarget"
				children: [
					#XmlElement & {name: "retract", ns: "urn:xmpp:message-retract:1"},
					#XmlElement & {name: "reason", ns: "urn:xmpp:message-moderate:1", text: "cleanup"},
				]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-moderate", type: "result"},
		#ExpectMessage & {
			target:     adminPhone
			bodyAbsent: true
			contains: [
				"urn:xmpp:message-moderate:1",
				"urn:xmpp:message-retract:1",
				"cleanup",
			]
		},
		#QueryMam & {
			actor:   adminPhone
			archive: roomJid
			id:      "cue-mam-moderation"
			idsFrom: ["moderationTarget"]
		},
		#ExpectMamResult & {
			bodyAbsent: true
			elements: [#XmlElement & {
				name: "retracted"
				ns:   "urn:xmpp:message-retract:1"
				children: [#XmlElement & {name: "moderated", ns: "urn:xmpp:message-moderate:1"}]
			}]
			absent: [moderateBody]
		},
		#DrainFrames & {
			target:   adminPhone
			contains: ["from='\(roomJid)'", "<subject></subject>"]
		},
		#DrainFrames & {
			target:   adminPhone
			contains: ["urn:xmpp:receipts", "cue-receipt-request"]
		},
		#DrainFrames & {
			target:   adminPhone
			contains: ["urn:xmpp:inbox:0", "cue-fulltext-nonmatch"]
		},
		#DrainFrames & {
			target:   adminPhone
			contains: ["urn:xmpp:inbox:0", "cue-reply"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["<presence", "from='\(roomJid)/admin'"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["from='\(roomJid)'", "<subject></subject>"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["urn:xmpp:inbox:0", "cue-rich-original"]
			min:      2
			max:      2
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["cue-fulltext-nonmatch"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["cue-reply"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["urn:xmpp:inbox:0", "cue-retract"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["id='cue-retract'", "urn:xmpp:message-retract:1"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["urn:xmpp:inbox:0", "cue-moderate-original"]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["id='cue-moderate-original'", moderateBody]
		},
		#DrainFrames & {
			target:   bobPhone
			contains: ["urn:xmpp:message-moderate:1", "cleanup"]
		},
	]
}
