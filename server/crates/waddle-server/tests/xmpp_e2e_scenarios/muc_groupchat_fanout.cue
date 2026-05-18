package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "muc-groupchat-fanout"
	xeps: ["XEP-0045"]
	users: {
		alice: devices: {
			phone: #Actor & {
				user:     "alice"
				device:   "phone"
				username: "admin"
				resource: "phone"
				domain:   scenario.domain
			}
			desktop: #Actor & {
				user:     "alice"
				device:   "desktop"
				username: "admin"
				resource: "desktop"
				domain:   scenario.domain
			}
		}
		bob: devices: phone: #Actor & {
			user:     "bob"
			device:   "phone"
			username: "bob"
			resource: "phone"
			domain:   scenario.domain
		}
	}

	let roomJid = "cue-fanout@muc.\(scenario.domain)"
	let alicePhone = users.alice.devices.phone
	let aliceDesktop = users.alice.devices.desktop
	let bobPhone = users.bob.devices.phone

	steps: [
		#JoinMuc & {actor: alicePhone, room: roomJid, nick: "alice-phone"},
		#ExpectFrame & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#JoinMuc & {actor: aliceDesktop, room: roomJid, nick: "alice-desktop"},
		#ExpectPresence & {
			target:   aliceDesktop
			contains: ["from=\"\(roomJid)/alice-phone\""]
		},
		#ExpectFrame & {
			target:   aliceDesktop
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#ExpectPresence & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)/alice-desktop\""]
		},
		#JoinMuc & {actor: bobPhone, room: roomJid, nick: "bob-phone"},
		#ExpectPresence & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)/alice-desktop\""]
		},
		#ExpectPresence & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)/alice-phone\""]
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["from=\"\(roomJid)\"", "<subject></subject>"]
		},
		#ExpectPresence & {
			target:   aliceDesktop
			contains: ["from=\"\(roomJid)/bob-phone\""]
		},
		#ExpectPresence & {
			target:   alicePhone
			contains: ["from=\"\(roomJid)/bob-phone\""]
		},
		#SendMessage & {
			from: alicePhone
			toJid: roomJid
			type:  "groupchat"
			id:    "cue-muc-fanout"
			body:  "muc fanout message to all joined devices"
		},
		#ExpectMessage & {
			target: alicePhone
			body:   "muc fanout message to all joined devices"
			contains: [roomJid]
		},
		#ExpectMessage & {
			target: aliceDesktop
			body:   "muc fanout message to all joined devices"
			contains: [roomJid]
		},
		#ExpectMessage & {
			target: bobPhone
			body:   "muc fanout message to all joined devices"
			contains: [roomJid]
		},
		#ExpectFrame & {
			target:   bobPhone
			contains: ["urn:xmpp:inbox:0", "cue-muc-fanout"]
		},
	]
}
