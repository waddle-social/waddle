package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0198-replay-gap-failure"
	xeps: ["XEP-0198"]
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

	steps: [
		#StreamManagement & {
			actor:  alicePhone
			action: "enable"
			resume: true
			max:    60
		},
		#ExpectFrame & {
			target:   alicePhone
			contains: ["urn:xmpp:sm:3"]
			elements: [#XmlElement & {
				name:         "enabled"
				ns:           "urn:xmpp:sm:3"
				attrs:        resume: "true"
				attrsPresent: ["id"]
			}]
			captures: [#AttributeCapture & {
				as:      "aliceSmId"
				name:    "id"
				element: "enabled"
				ns:      "urn:xmpp:sm:3"
			}]
		},
		#SendMessageBurst & {
			from:       bobPhone
			to:         alicePhone
			idPrefix:   "cue-sm-overflow"
			bodyPrefix: "stream-management-overflow"
			count:      1001
		},
		#DrainFrames & {
			target:   alicePhone
			contains: ["stream-management-overflow"]
			millis:   20000
			min:      1001
			max:      1001
		},
		#DisconnectActor & {
			actor:    alicePhone
			graceful: false
		},
		#ConnectActor & {
			actor: alicePhone
			bind:  false
		},
		#StreamManagement & {
			actor:      alicePhone
			action:     "resume"
			previdFrom: "aliceSmId"
			h:          0
		},
		#ExpectFrame & {
			target:   alicePhone
			contains: ["urn:xmpp:sm:3", "resource-constraint"]
			elements: [
				#XmlElement & {
					name:         "failed"
					ns:           "urn:xmpp:sm:3"
					attrsPresent: ["h"]
				},
				#XmlElement & {
					name: "resource-constraint"
					ns:   "urn:ietf:params:xml:ns:xmpp-stanzas"
				},
			]
		},
	]
}
