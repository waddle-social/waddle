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
	name:        string
	description?: string
	xeps?: [...#XepId]
	domain:      *"localhost" | string
	users:       [string]: #User
	steps:      [...#Step]
	...
}

#XepId: =~"^XEP-[0-9]{4}$"

#Step:
	#EnableCarbons |
	#StreamManagement |
	#ConnectActor |
	#DisconnectActor |
	#SendIq |
	#ExpectIq |
	#SendPresence |
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
	#ExpectNoMamResult |
	#ExpectFrame |
	#DrainFrames |
	#ExpectNoStanza

#EnableCarbons: {
	kind:  "enableCarbons"
	actor: #Actor
	...
}

#StreamManagement: {
	kind:   "streamManagement"
	actor:  #Actor
	action: "enable" | "requestAck"
	resume?: bool
	max?:    int & >=1 & <=4294967295
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

#SendIq: {
	kind:  "sendIq"
	actor: #Actor
	type:  "get" | "set" | "result"
	id?:   string
	to?:   string
	payload?: #XmlElement
	...
}

#ExpectIq: {
	kind:   "expectIq"
	target: #Actor
	id?:    string
	type?:  "result" | "error" | "get" | "set"
	contains?: [...string]
	absent?: [...string]
	elements?: [...#XmlElement]
	absentElements?: [...#XmlElement]
	captures?: [...#AttributeCapture]
	...
}

#SendPresence: {
	kind:   "sendPresence"
	actor:  #Actor
	to?:    string
	type?:  "available" | "unavailable" | "subscribe" | "subscribed" | "unsubscribe" | "unsubscribed" | "probe"
	show?:  "away" | "chat" | "dnd" | "xa"
	status?: string
	priority?: int & >=-128 & <=127
	payloads?: [...#XmlElement]
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
	absent?: [...string]
	elements?: [...#XmlElement]
	absentElements?: [...#XmlElement]
	...
}

#ExpectCarbon: {
	kind:        "expectCarbon"
	target:      #Actor
	carbon:      "sent" | "received"
	#BodyExpectation
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	absent?: [...string]
	elements?: [...#XmlElement]
	absentElements?: [...#XmlElement]
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
	contains?: [...string]
	elements?: [...#XmlElement]
	absentElements?: [...#XmlElement]
	captures?: [...#AttributeCapture]
	...
}

#QueryMam: {
	kind:    "queryMam"
	actor:   #Actor
	archive: string
	id?:     string
	max:     *50 | int & >=1 & <=4294967295
	after?:  string
	with?:   string
	fulltext?: string
	ids?: [...string]
	idsFrom?: [...string]
	...
}

#ExpectMamResult: {
	kind:        "expectMamResult"
	#BodyExpectation
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	absent?: [...string]
	elements?: [...#XmlElement]
	absentElements?: [...#XmlElement]
	...
}

#ExpectNoMamResult: {
	kind: "expectNoMamResult"
	#BodyExpectation
	payloads?: [...#ExpectedPayload]
	contains?: [...string]
	elements?: [...#XmlElement]
	...
}

#ExpectFrame: {
	kind: "expectFrame"
	target: #Actor
	contains: [...string] & [string, ...string]
	absent?: [...string]
	elements?: [...#XmlElement]
	absentElements?: [...#XmlElement]
	...
}

#DrainFrames: {
	kind: "drainFrames"
	target: #Actor
	contains: [...string] & [string, ...string]
	elements?: [...#XmlElement]
	millis: *250 | int & >=1
	min?:    int & >=0
	max?:    int & >=0
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

#Payload: #FileShare | #LinkMetadata | #MessageCorrection | #Reactions | #ProcessingHint | #PinAttachment | #XmlPayload

#ExpectedPayload: #FileShare | #LinkMetadata | #MessageCorrection | #Reactions | #ProcessingHint | #PinAttachment | #PinEvent | #XmlPayload

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

#XmlPayload: {
	kind:    "xml"
	element: #XmlElement
	expectElements?: [...#XmlElement]
	...
}

#XmlElement: {
	name: string
	ns:   string
	attrs?: [string]: string
	attrsFrom?: [string]: string
	attrsPresent?: [...string]
	text?: string
	children?: [...#XmlElement]
	...
}

#AttributeCapture: {
	as: string
	name: string
	element?: string
	ns?: string
	contains?: string
	...
}
