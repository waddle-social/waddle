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
	#ConnectActor |
	#DisconnectActor |
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

#ConnectActor: {
	kind:  "connectActor"
	actor: #Actor
	...
}

#DisconnectActor: {
	kind:  "disconnectActor"
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
	kind:              "expectMessage"
	target:            #Actor
	#BodyExpectation
	from?:             #Actor
	captureStanzaIdAs?: string
	captureStanzaIdBy?: string
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	...
}

#ExpectCarbon: {
	kind:        "expectCarbon"
	target:      #Actor
	carbon:      "sent" | "received"
	#BodyExpectation
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
	kind:        "expectMamResult"
	#BodyExpectation
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	...
}

#BodyExpectation: {
	body?:       string
	bodyAbsent?: false
} | {
	body?:       _|_
	bodyAbsent: true
}

#ExpectNoStanza: {
	kind:   "expectNoStanza"
	target: #Actor
	body?:  string
	contains?: [...string]
	millis: *250 | int & >=1
	...
}

#Payload: #FileShare | #LinkMetadata | #MessageCorrection | #Reactions | #ProcessingHint | #PinAttachment

#ExpectedPayload: #FileShare | #LinkMetadata | #MessageCorrection | #Reactions | #ProcessingHint | #PinAttachment | #PinEvent

#MessageCorrection: {
	kind: "messageCorrection"
	id:   string
	...
}

#Reactions: {
	kind: "reactions"
	(#ReactionId | #ReactionIdFrom)
	emojis: [...string]
	...
}

#ReactionId: {
	id: string
	idFrom?: _|_
}

#ReactionIdFrom: {
	id?: _|_
	idFrom: string
}

#ProcessingHint: {
	kind: "processingHint"
	name: "no-permanent-store" | "no-store" | "no-copy" | "store"
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

#PinAttachment: {
	kind:   "pinAttachment"
	(#PinId | #PinIdFrom)
	action: "pinned" | "unpinned"
	...
}

#PinId: {
	id: string
	idFrom?: _|_
}

#PinIdFrom: {
	id?: _|_
	idFrom: string
}

#PinEvent: {
	kind:   "pinEvent"
	(#PinId | #PinIdFrom)
	action: "pinned" | "unpinned"
	...
}
