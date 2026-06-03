package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0313-reconnect-after-catchup"
	xeps: ["XEP-0045", "XEP-0059", "XEP-0160", "XEP-0297", "XEP-0313"]
	users: {
			alice: devices: phone: #Actor & {
				user:     "alice"
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

	let alicePhone = users.alice.devices.phone
	let bobPhone = users.bob.devices.phone
	let roomJid = "cue-reconnect-after@muc.\(scenario.domain)"

	steps: [
		#SendMessage & {
			from: alicePhone
			to:   bobPhone
			id:   "cue-before-gap"
			body: "before reconnect gap"
		},
		#ExpectMessage & {
			target: bobPhone
			from:   alicePhone
			body:   "before reconnect gap"
		},
		#QueryMam & {
			actor:   bobPhone
			archive: bobPhone.bareJid
			id:      "cue-baseline-mam"
			max:     1
		},
		#ExpectMamResult & {
			body: "before reconnect gap"
			captures: [#AttributeCapture & {
				as:      "baselineMamId"
				element: "result"
				ns:      "urn:xmpp:mam:2"
				name:    "id"
			}]
		},
		#DisconnectActor & {
			actor: bobPhone
		},
		#SendMessage & {
			from:  alicePhone
			toJid: bobPhone.bareJid
			id:    "cue-gap-one"
			body:  "missed reconnect gap one"
		},
		#SendMessage & {
			from:  alicePhone
			toJid: bobPhone.bareJid
			id:    "cue-gap-two"
			body:  "missed reconnect gap two"
		},
		#ConnectActor & {
			actor: bobPhone
		},
		#SendPresence & {
			actor: bobPhone
		},
		#ExpectMessage & {
			target: bobPhone
			body:   "missed reconnect gap one"
			elements: [#XmlElement & {
				name: "delay"
				ns:   "urn:xmpp:delay"
				attrsPresent: ["stamp"]
			}]
		},
		#ExpectMessage & {
			target: bobPhone
			body:   "missed reconnect gap two"
			elements: [#XmlElement & {
				name: "delay"
				ns:   "urn:xmpp:delay"
				attrsPresent: ["stamp"]
			}]
		},
		#QueryMam & {
			actor:     bobPhone
			archive:   bobPhone.bareJid
			id:        "cue-after-baseline"
			max:       1
			afterFrom: "baselineMamId"
		},
		#ExpectMamResult & {
			body: "missed reconnect gap one"
			captures: [#AttributeCapture & {
				as:      "gapOneMamId"
				element: "result"
				ns:      "urn:xmpp:mam:2"
				name:    "id"
			}]
		},
		#ExpectNoMamResult & {
			body: "missed reconnect gap two"
		},
		#QueryMam & {
			actor:     bobPhone
			archive:   bobPhone.bareJid
			id:        "cue-after-gap-one"
			max:       1
			afterFrom: "gapOneMamId"
		},
			#ExpectMamResult & {
				body: "missed reconnect gap two"
			},
			#QueryMam & {
				actor:   bobPhone
				archive: bobPhone.bareJid
				id:      "cue-latest-before-walk"
				max:     1
				before:  ""
			},
			#ExpectMamResult & {
				body: "missed reconnect gap two"
				captures: [#AttributeCapture & {
					as:      "gapTwoMamId"
					element: "result"
					ns:      "urn:xmpp:mam:2"
					name:    "id"
				}]
			},
			#QueryMam & {
				actor:      bobPhone
				archive:    bobPhone.bareJid
				id:         "cue-before-gap-two"
				max:        1
				beforeFrom: "gapTwoMamId"
			},
			#ExpectMamResult & {
				body: "missed reconnect gap one"
			},
			#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice-phone"},
			#ExpectFrame & {
				target:   alicePhone
				contains: ["from='\(roomJid)'", "<subject></subject>"]
			},
			#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob-phone"},
			#ExpectPresence & {
				target:   bobPhone
				contains: ["from='\(roomJid)/alice-phone'"]
			},
			#ExpectFrame & {
				target:   bobPhone
				contains: ["from='\(roomJid)'", "<subject></subject>"]
			},
			#ExpectPresence & {
				target:   alicePhone
				contains: ["from='\(roomJid)/bob-phone'"]
			},
			#SendMessage & {
				from:  alicePhone
				toJid: roomJid
				type:  "groupchat"
				id:    "cue-room-before-gap"
				body:  "room before reconnect gap"
			},
			#ExpectMessage & {
				target: alicePhone
				body:   "room before reconnect gap"
			},
			#ExpectMessage & {
				target: bobPhone
				body:   "room before reconnect gap"
			},
			#ExpectFrame & {
				target:   bobPhone
				contains: ["urn:xmpp:inbox:1", "cue-room-before-gap"]
			},
			#QueryMam & {
				actor:   bobPhone
				archive: roomJid
				id:      "cue-room-baseline-mam"
				max:     1
				before:  ""
			},
			#ExpectMamResult & {
				body: "room before reconnect gap"
				elements: [#XmlElement & {
					name: "stanza-id"
					ns:   "urn:xmpp:sid:0"
					attrs: by: roomJid
					attrsPresent: ["id"]
				}]
				captures: [#AttributeCapture & {
					as:      "roomBaselineMamId"
					element: "result"
					ns:      "urn:xmpp:mam:2"
					name:    "id"
				}]
			},
			#DisconnectActor & {
				actor: bobPhone
			},
			#SendMessage & {
				from:  alicePhone
				toJid: roomJid
				type:  "groupchat"
				id:    "cue-room-gap-one"
				body:  "room missed reconnect gap one"
			},
			#ExpectMessage & {
				target: alicePhone
				body:   "room missed reconnect gap one"
			},
			#SendMessage & {
				from:  alicePhone
				toJid: roomJid
				type:  "groupchat"
				id:    "cue-room-gap-two"
				body:  "room missed reconnect gap two"
			},
			#ExpectMessage & {
				target: alicePhone
				body:   "room missed reconnect gap two"
			},
			#ConnectActor & {
				actor: bobPhone
			},
			#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob-phone"},
			#ExpectPresence & {
				target:   bobPhone
				contains: ["from='\(roomJid)/alice-phone'"]
			},
			#ExpectFrame & {
				target:   bobPhone
				contains: ["from='\(roomJid)'", "<subject></subject>"]
			},
			#ExpectPresence & {
				target:   alicePhone
				contains: ["from='\(roomJid)/bob-phone'"]
			},
			#ExpectNoStanza & {
				target: bobPhone
				body:   "room missed reconnect gap one"
				millis: 250
			},
			#ExpectNoStanza & {
				target: bobPhone
				body:   "room missed reconnect gap two"
				millis: 250
			},
			#QueryMam & {
				actor:     bobPhone
				archive:   roomJid
				id:        "cue-room-after-baseline"
				max:       1
				afterFrom: "roomBaselineMamId"
			},
			#ExpectMamResult & {
				body: "room missed reconnect gap one"
				elements: [#XmlElement & {
					name: "stanza-id"
					ns:   "urn:xmpp:sid:0"
					attrs: by: roomJid
					attrsPresent: ["id"]
				}]
				captures: [#AttributeCapture & {
					as:      "roomGapOneMamId"
					element: "result"
					ns:      "urn:xmpp:mam:2"
					name:    "id"
				}]
			},
			#ExpectNoMamResult & {
				body: "room missed reconnect gap two"
			},
			#QueryMam & {
				actor:     bobPhone
				archive:   roomJid
				id:        "cue-room-after-gap-one"
				max:       1
				afterFrom: "roomGapOneMamId"
			},
			#ExpectMamResult & {
				body: "room missed reconnect gap two"
				elements: [#XmlElement & {
					name: "stanza-id"
					ns:   "urn:xmpp:sid:0"
					attrs: by: roomJid
					attrsPresent: ["id"]
				}]
			},
		]
	}
