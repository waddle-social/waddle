package xmpp_e2e_scenarios

#Actor: {
	user:     string
	device:   string
	username: string
	resource: string
	domain:   string
	bareJid:  "\(username)@\(domain)"
	jid:      "\(bareJid)/\(resource)"
	...
}

#User: {
	devices: [string]: #Actor
	...
}

#Scenario: {
	name:   string
	domain: *"localhost" | string
	users: [string]: #User
	steps: [...#Step]
	...
}

#Step:
	#EnableCarbons |
	#SendMessage |
	#ExpectMessage |
	#ExpectCarbon |
	#JoinMuc |
	#SetMucAffiliation |
	#ExpectMucAffiliation |
	#ExpectMucAdminDenied |
	#ExpectPresence |
	#QueryMam |
	#ExpectMamResult |
	#ExpectNoStanza

#EnableCarbons: {
	kind:  "enableCarbons"
	actor: #Actor
	...
}

#SendMessage: {
	kind: "sendMessage"
	from: #Actor
	#Destination
	type: *"chat" | "normal" | "groupchat"
	id?:  string
	body?: string
	payloads?: [...#Payload]
	...
}

#Destination: (#ActorDestination | #JidDestination)

#ActorDestination: {
	to: #Actor
	toJid?: _|_
}

#JidDestination: {
	to?: _|_
	toJid: string
}

#ExpectMessage: {
	kind:   "expectMessage"
	target: #Actor
	body?:  string
	from?:  #Actor
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	...
}

#ExpectCarbon: {
	kind:   "expectCarbon"
	target: #Actor
	carbon: "sent" | "received"
	body?:  string
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	...
}

#JoinMuc: {
	kind:  "joinMuc"
	actor: #Actor
	room:  string
	nick:  *actor.username | string
	...
}

#SetMucAffiliation: {
	kind:        "setMucAffiliation"
	actor:       #Actor
	room:        string
	jid:         string
	affiliation: #MucAffiliation
	id?:         string
	...
}

#ExpectMucAffiliation: {
	kind:        "expectMucAffiliation"
	actor:       #Actor
	room:        string
	jid:         string
	affiliation: #MucAffiliation
	id?:         string
	...
}

#ExpectMucAdminDenied: {
	kind:        "expectMucAdminDenied"
	actor:       #Actor
	room:        string
	jid:         string
	affiliation: #MucAffiliation
	id?:         string
	...
}

#MucAffiliation: "owner" | "admin" | "member" | "none" | "outcast"

#ExpectPresence: {
	kind:   "expectPresence"
	target: #Actor
	contains: [...string] & [string, ...string]
	...
}

#QueryMam: {
	kind:    "queryMam"
	actor:   #Actor
	archive: string
	id?:     string
	max:     *50 | int & >=1
	...
}

#ExpectMamResult: {
	kind: "expectMamResult"
	body?: string
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	...
}

#ExpectNoStanza: {
	kind:   "expectNoStanza"
	target: #Actor
	body?:  string
	contains?: [...string]
	millis: *250 | int & >=1
	...
}

#Payload: #FileShare | #LinkMetadata | #MessageCorrection

#ExpectedPayload: #FileShare | #LinkMetadata | #MessageCorrection

#MessageCorrection: {
	kind: "messageCorrection"
	id:   string
	...
}

#FileShare: {
	kind:        "fileShare"
	disposition: *"inline" | "attachment"
	name:        string
	mediaType:   string
	size:        int & >=0
	url:         string
	...
}

#LinkMetadata: {
	kind:        "linkMetadata"
	about:       string
	title:       string
	description: string
	url:         string
	...
}
