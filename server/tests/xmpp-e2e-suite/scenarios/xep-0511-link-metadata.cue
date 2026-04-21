package xmpp_e2e_suite

scenario: {
	name: "xep-0511-link-metadata"
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
				stanza: "<message xmlns='jabber:client' type='chat' id='xep-0511-1' to='\(scenario.users.bob.devices.phone.username)@\(domain)/\(scenario.users.bob.devices.phone.resource)'><body>sharing a useful link: https://the.link.example.com/what-was-linked-to</body><rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://the.link.example.com/what-was-linked-to'><og:title>The Best Webpage</og:title><og:description>This is a great webpage and you will really like it</og:description><og:url>https://example.com/canonical-url/for/what-was-linked-to</og:url></rdf:Description></message>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "bob"
				device: "phone"
			}
			contains: [
				"<body>sharing a useful link: https://the.link.example.com/what-was-linked-to</body>",
				"<rdf:Description",
				"<title xmlns='https://ogp.me/ns#'>The Best Webpage</title>",
				"https://example.com/canonical-url/for/what-was-linked-to",
			]
		},
		#ExpectMamRows & {
			body: "sharing a useful link: https://the.link.example.com/what-was-linked-to"
		},
	]
}
