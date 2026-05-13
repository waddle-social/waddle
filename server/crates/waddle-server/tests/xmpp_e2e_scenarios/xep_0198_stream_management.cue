package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0198-stream-management"
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
		},
		#SendMessage & {
			from: alicePhone
			to:   bobPhone
			id:   "cue-sm-counted"
			body: "stream-management-counted"
		},
		#ExpectMessage & {
			target: bobPhone
			from:   alicePhone
			body:   "stream-management-counted"
		},
		#StreamManagement & {
			actor:  alicePhone
			action: "requestAck"
		},
		#ExpectFrame & {
			target:   alicePhone
			contains: ["urn:xmpp:sm:3"]
			elements: [#XmlElement & {
				name: "a"
				ns:   "urn:xmpp:sm:3"
				attrs: h: "1"
			}]
		},
	]
}
