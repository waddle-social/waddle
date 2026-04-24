package xmpp_e2e_suite

scenario: {
	name: "muc-managed-membership-spicedb"
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
		carol: #User & {
			devices: {
				phone: #Device & {
					username: "carol"
					resource: "phone"
				}
			}
		}
		dave: #User & {
			devices: {
				phone: #Device & {
					username: "dave"
					resource: "phone"
				}
			}
		}
	}

	fixtures: #ScenarioFixtures & {
		channels: [{
			waddleId:   "waddle-1"
			channelId:  "general"
			channelName: "General"
			channelType: "text"
		}]
		permissionGrants: [
			{
				resource: "space:waddle-1"
				relation: "owner"
				subject:  "user:user-test-alice"
			},
			{
				resource: "space:waddle-1"
				relation: "admin"
				subject:  "user:user-test-bob"
			},
			{
				resource: "space:waddle-1"
				relation: "member"
				subject:  "user:user-test-carol"
			},
		]
	}

	steps: [
		{
			send: {
				actor: {
					user:   "alice"
					device: "phone"
				}
				stanza: "<presence to='waddle-1_general@muc.\(domain)/alice' xmlns='jabber:client'><x xmlns='http://jabber.org/protocol/muc'><history maxstanzas='0'/></x></presence>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "phone"
			}
			until: "</presence>"
			contains: [
				"status code=\"110\"",
				"affiliation=\"owner\"",
				"role=\"moderator\"",
			]
		},
		{
			send: {
				actor: {
					user:   "bob"
					device: "phone"
				}
				stanza: "<presence to='waddle-1_general@muc.\(domain)/bob' xmlns='jabber:client'><x xmlns='http://jabber.org/protocol/muc'><history maxstanzas='0'/></x></presence>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "phone"
			}
			until: "</presence>"
			contains: [
				"bob@localhost",
				"affiliation=\"admin\"",
				"role=\"moderator\"",
			]
		},
		{
			send: {
				actor: {
					user:   "carol"
					device: "phone"
				}
				stanza: "<presence to='waddle-1_general@muc.\(domain)/carol' xmlns='jabber:client'><x xmlns='http://jabber.org/protocol/muc'><history maxstanzas='0'/></x></presence>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "phone"
			}
			until: "</presence>"
			contains: [
				"carol@localhost",
				"affiliation=\"member\"",
				"role=\"participant\"",
			]
		},
		{
			send: {
				actor: {
					user:   "dave"
					device: "phone"
				}
				stanza: "<presence to='waddle-1_general@muc.\(domain)/dave' xmlns='jabber:client'><x xmlns='http://jabber.org/protocol/muc'><history maxstanzas='0'/></x></presence>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "dave"
				device: "phone"
			}
			until: "</presence>"
			contains: [
				"type=\"error\"",
				"registration-required",
			]
		},
		{
			send: {
				actor: {
					user:   "alice"
					device: "phone"
				}
				stanza: "<iq xmlns='jabber:client' type='get' id='list-members' from='alice@\(domain)/phone' to='waddle-1_general@muc.\(domain)'><query xmlns='http://jabber.org/protocol/muc#admin'><item affiliation='member'/></query></iq>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "phone"
			}
			until: "</iq>"
			contains: [
				"id=\"list-members\"",
				"type=\"result\"",
				"carol@localhost",
				"affiliation=\"member\"",
			]
		},
		{
			send: {
				actor: {
					user:   "alice"
					device: "phone"
				}
				stanza: "<iq xmlns='jabber:client' type='get' id='list-admins' from='alice@\(domain)/phone' to='waddle-1_general@muc.\(domain)'><query xmlns='http://jabber.org/protocol/muc#admin'><item affiliation='admin'/></query></iq>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "phone"
			}
			until: "</iq>"
			contains: [
				"id=\"list-admins\"",
				"bob@localhost",
				"affiliation=\"admin\"",
			]
		},
		{
			send: {
				actor: {
					user:   "alice"
					device: "phone"
				}
				stanza: "<iq xmlns='jabber:client' type='get' id='list-owners' from='alice@\(domain)/phone' to='waddle-1_general@muc.\(domain)'><query xmlns='http://jabber.org/protocol/muc#admin'><item affiliation='owner'/></query></iq>"
			}
		},
		#ExpectContains & {
			target: {
				user:   "alice"
				device: "phone"
			}
			until: "</iq>"
			contains: [
				"id=\"list-owners\"",
				"alice@localhost",
				"affiliation=\"owner\"",
			]
		},
	]
}
