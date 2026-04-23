package xmpp_e2e_suite

scenario: {
	name: "xep-0447-stateless-file-sharing"
	domain: "localhost"
	users: {
		alice: #User & {
			devices: {
				phone: #Device & {
					username: "alice"
					resource: "phone"
				}
			}
		}
		bob: #User & {
			devices: {
				phone: #Device & {
					username: "bob"
					resource: "phone"
				}
			}
		}
	}

	steps: [
		{
			send: {
				actor: {
					user:   "alice"
					device: "phone"
				}
				stanza: "<message xmlns='jabber:client' type='chat' id='xep-0447-1' to='\(scenario.users.bob.devices.phone.username)@\(domain)/\(scenario.users.bob.devices.phone.resource)'><body>https://files.example.com/report.pdf</body><file-sharing xmlns='urn:xmpp:sfs:0' disposition='inline'><file xmlns='urn:xmpp:file:metadata:0'><media-type>application/pdf</media-type><name>report.pdf</name><size>4096</size></file><sources><url-data xmlns='http://jabber.org/protocol/url-data' target='https://files.example.com/report.pdf'/></sources></file-sharing></message>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "bob"
				device: "phone"
			}
			contains: [
				"<body>https://files.example.com/report.pdf</body>",
				"<file-sharing xmlns='urn:xmpp:sfs:0' disposition='inline'>",
				"<name xmlns='urn:xmpp:file:metadata:0'>report.pdf</name>",
				"<size xmlns='urn:xmpp:file:metadata:0'>4096</size>",
				"target='https://files.example.com/report.pdf'",
			]
		},
		#ExpectMamRows & {
			body: "https://files.example.com/report.pdf"
		},
	]
}
