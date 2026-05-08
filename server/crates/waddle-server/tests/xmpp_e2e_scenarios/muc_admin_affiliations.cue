package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "muc-admin-affiliations"
	xeps: ["XEP-0045"]
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

	let roomJid = "cue-admin-affiliations@muc.\(scenario.domain)"
	let alicePhone = users.alice.devices.phone
	let bobPhone = users.bob.devices.phone

	steps: [
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice"},
		#ExpectFrame & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#SetMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "member"
			id:          "cue-muc-set-member"
		},
		#ExpectMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "member"
			id:          "cue-muc-query-member"
		},
		#SetMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "admin"
			id:          "cue-muc-set-admin"
		},
		#ExpectMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "admin"
			id:          "cue-muc-query-admin"
		},
		#SetMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "owner"
			id:          "cue-muc-set-owner"
		},
		#ExpectMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "owner"
			id:          "cue-muc-query-owner"
		},
		#SetMucAffiliation & {
			actor:       alicePhone
			room:        roomJid
			jid:         bobPhone.bareJid
			affiliation: "member"
			id:          "cue-muc-restore-member"
		},
		#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob"},
		#ExpectPresence & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)/alice\""]
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#ExpectPresence & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)/bob\""]
		},
		#ExpectMucAdminDenied & {
			actor:       bobPhone
			room:        roomJid
			jid:         alicePhone.bareJid
			affiliation: "outcast"
			id:          "cue-muc-member-cannot-admin"
		},
	]
}
