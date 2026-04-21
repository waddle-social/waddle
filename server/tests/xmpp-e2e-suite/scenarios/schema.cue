package xmpp_e2e_suite

#Device: {
	id:       string
	username: string
	resource: string
	...
}

#User: {
	id:      string
	devices: [...#Device]
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
	actor:   string
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
	target:   string
	contains: [...string] & [string, ...string]
	let targetRef = target
	let containsRef = contains
	expectStanza: {
		target:   targetRef
		contains: containsRef
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
