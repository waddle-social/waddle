package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-0447-stateless-file-sharing"
	xeps: ["XEP-0103", "XEP-0313", "XEP-0428", "XEP-0446", "XEP-0447"]
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
	let fileUrl = "https://files.example.com/report.pdf"

	steps: [
		#SendMessage & {
			from: alicePhone
			to:   bobPhone
			id:   "cue-xep-0447"
			body: fileUrl
			payloads: [
				#FileShare & {
					name:      "report.pdf"
					mediaType: "application/pdf"
					size:      4096
					url:       fileUrl
				},
			]
		},
		#ExpectMessage & {
			target: bobPhone
			body:   fileUrl
			payloads: [
				#FileShare & {
					name:      "report.pdf"
					mediaType: "application/pdf"
					size:      4096
					url:       fileUrl
				},
			]
		},
		#QueryMam & {
			actor:   alicePhone
			archive: alicePhone.bareJid
			id:      "cue-xep-0447-mam"
		},
		#ExpectMamResult & {
			body: fileUrl
			payloads: [
				#FileShare & {
					name:      "report.pdf"
					mediaType: "application/pdf"
					size:      4096
					url:       fileUrl
				},
			]
		},
	]
}
