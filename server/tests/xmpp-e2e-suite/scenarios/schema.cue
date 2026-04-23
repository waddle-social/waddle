package xmpp_e2e_suite

#Device: {
	username: string
	resource: string
	...
}

#User: {
	devices: [string]: #Device
	...
}

#ActorRef: {
	user:   string
	device: string
	...
}

#Message: {
	to:    string
	type:  *"chat" | string
	id:    string
	body:  string
	xmlns: *"jabber:client" | string

	stanza: "<message to='\(to)' type='\(type)' id='\(id)' xmlns='\(xmlns)'><body>\(body)</body></message>"
	...
}

#SendMessage: {
	actor:   #ActorRef
	message: #Message
	let actorRef = actor
	let stanzaRef = message.stanza
	send: {
		actor:  actorRef
		stanza: stanzaRef
		...
	}
	...
}

#ExpectContains: {
	target:   #ActorRef
	contains: [...string] & [string, ...string]
	until:    *"</message>" | string
	let targetRef = target
	let containsRef = contains
	let untilRef = until
	expectStanza: {
		target:   targetRef
		contains: containsRef
		until:    untilRef
		...
	}
	...
}

#ExpectMamRows: {
	body:    string
	minRows: *1 | int & >=1
	let bodyRef = body
	let minRowsRef = minRows
	expectDb: {
		table: "mam_messages"
		where: {
			body: bodyRef
			...
		}
		minRows: minRowsRef
		...
	}
	...
}

#ChannelFixture: {
	waddleId:   string
	channelId:  string
	channelName: string
	channelType: string
	...
}

#PermissionGrant: {
	resource: string
	relation: string
	subject:  string
	...
}

#ScenarioFixtures: {
	channels?: [...#ChannelFixture]
	permissionGrants?: [...#PermissionGrant]
	...
}
