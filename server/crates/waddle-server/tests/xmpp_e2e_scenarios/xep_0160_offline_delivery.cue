package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0160-offline-delivery"
	xeps: ["XEP-0160", "XEP-0199", "XEP-0203"]
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
		#DisconnectActor & {actor: bobPhone},
		#SendMessage & {
			from:  alicePhone
			toJid: bobPhone.bareJid
			id:    "cue-offline-message"
			body:  "offline delivery from cue"
		},
		// The sender-stream reply proves the preceding offline intake finished.
		#SendIq & {
			actor: alicePhone
			type:  "get"
			id:    "cue-offline-intake-barrier"
			to:    scenario.domain
			payload: #XmlElement & {name: "ping", ns: "urn:xmpp:ping"}
		},
		#ExpectIq & {target: alicePhone, type: "result", id: "cue-offline-intake-barrier"},
		#ConnectActor & {actor: bobPhone},
		#SendPresence & {actor: bobPhone},
		#ExpectMessage & {
			target: bobPhone
			body:   "offline delivery from cue"
			elements: [#XmlElement & {
				name:         "delay"
				ns:           "urn:xmpp:delay"
				attrsPresent: ["stamp"]
			}]
		},
	]
}
