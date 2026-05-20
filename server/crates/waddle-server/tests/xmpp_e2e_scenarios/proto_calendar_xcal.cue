package xmpp_e2e_scenarios

// Calendar events use the XSF ProtoXEP "Calendaring Extensions to
// Publish-Subscribe" wire shape: xCal (`urn:ietf:params:xml:ns:xcal`)
// payloads carrying iCalendar VEVENT components. The ProtoXEP has no
// assigned XEP number yet, so we tag this scenario with the
// PROTO-CALENDAR label that the cuenv coverage map recognises.
scenario: #Scenario & {
	name: "proto-calendar-xcal"
	xeps: ["XEP-0060", "PROTO-CALENDAR"]
	users: admin: devices: phone: #Actor & {
		user:     "admin"
		device:   "phone"
		username: "admin"
		resource: "phone"
		domain:   scenario.domain
	}

	let adminPhone = users.admin.devices.phone

	steps: [
		// 1. disco#info on the community service MUST advertise
		//    the xCal namespace so clients can discover support
		//    before subscribing or publishing.
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-calendar-disco"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-calendar-disco"
			type:   "result"
			contains: [
				"var='urn:ietf:params:xml:ns:xcal'",
			]
		},
		// 2. Publish a recurring event in xCal form. The VCALENDAR
		//    wraps a single VEVENT with an RRULE element specifying
		//    weekly recurrence on Fridays, terminating after 10
		//    occurrences.
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-event-publish"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "publish"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:calendar:0"
						children: [
							#XmlElement & {
								name:  "item"
								ns:    "http://jabber.org/protocol/pubsub"
								attrs: id: "cue-event-weekly"
								children: [
									#XmlElement & {
										name: "vcalendar"
										ns:   "urn:ietf:params:xml:ns:xcal"
										children: [
											#XmlElement & {
												name: "version"
												ns:   "urn:ietf:params:xml:ns:xcal"
												text: "2.0"
											},
											#XmlElement & {
												name: "vevent"
												ns:   "urn:ietf:params:xml:ns:xcal"
												children: [
													#XmlElement & {
														name: "uid"
														ns:   "urn:ietf:params:xml:ns:xcal"
														text: "cue-event-weekly"
													},
													#XmlElement & {
														name: "dtstart"
														ns:   "urn:ietf:params:xml:ns:xcal"
														text: "2026-06-05T19:00:00Z"
													},
													#XmlElement & {
														name: "dtend"
														ns:   "urn:ietf:params:xml:ns:xcal"
														text: "2026-06-05T22:00:00Z"
													},
													#XmlElement & {
														name: "summary"
														ns:   "urn:ietf:params:xml:ns:xcal"
														text: "Friday Game Night"
													},
													#XmlElement & {
														name: "organizer"
														ns:   "urn:ietf:params:xml:ns:xcal"
														text: "xmpp:admin@\(scenario.domain)"
													},
													#XmlElement & {
														name: "rrule"
														ns:   "urn:ietf:params:xml:ns:xcal"
														children: [
															#XmlElement & {
																name: "freq"
																ns:   "urn:ietf:params:xml:ns:xcal"
																text: "WEEKLY"
															},
															#XmlElement & {
																name: "interval"
																ns:   "urn:ietf:params:xml:ns:xcal"
																text: "1"
															},
															#XmlElement & {
																name: "byday"
																ns:   "urn:ietf:params:xml:ns:xcal"
																children: [
																	#XmlElement & {
																		name: "weekday"
																		ns:   "urn:ietf:params:xml:ns:xcal"
																		text: "FR"
																	},
																]
															},
															#XmlElement & {
																name: "count"
																ns:   "urn:ietf:params:xml:ns:xcal"
																text: "10"
															},
														]
													},
												]
											},
										]
									},
								]
							},
						]
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-event-publish"
			type:   "result"
		},
		// 3. Items query MUST return the published VEVENT with its
		//    typed payload intact (UID, summary, DTSTART, RRULE
		//    with FREQ=WEEKLY/BYDAY=FR/COUNT=10).
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-event-items"
			to:    "community.\(scenario.domain)"
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "items"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:calendar:0"
					},
				]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-event-items"
			type:   "result"
			contains: [
				"id='cue-event-weekly'",
				"urn:ietf:params:xml:ns:xcal",
				"<vevent",
				"Friday Game Night",
				"2026-06-05T19:00:00Z",
				"WEEKLY",
				"FR",
				"10",
			]
		},
	]
}
