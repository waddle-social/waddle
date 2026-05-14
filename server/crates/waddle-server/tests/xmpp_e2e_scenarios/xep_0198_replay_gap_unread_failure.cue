package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0198-replay-gap-unread-failure"
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
			target: alicePhone
			contains: ["urn:xmpp:sm:3"]
			elements: [#XmlElement & {
				name: "enabled"
				ns:   "urn:xmpp:sm:3"
				attrs: resume: "true"
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
			idPrefix:   "cue-sm-unread-overflow"
			bodyPrefix: "stream-management-unread-overflow"
			count:      1100
		},
		#WaitMillis & {
			millis: 1500
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
			target: alicePhone
			contains: ["urn:xmpp:sm:3", "resource-constraint"]
			elements: [
				#XmlElement & {
					name: "failed"
					ns:   "urn:xmpp:sm:3"
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
