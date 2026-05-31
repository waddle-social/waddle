package xmpp_e2e_scenarios

scenario: #Scenario & {
	name: "xep-storage-pubsub-pep"
	xeps: [
		"XEP-0048",
		"XEP-0049",
		"XEP-0054",
		"XEP-0060",
		"XEP-0084",
		"XEP-0107",
		"XEP-0108",
		"XEP-0118",
		"XEP-0153",
		"XEP-0163",
		"XEP-0191",
		"XEP-0402",
		"XEP-0492",
	]
	users: {
		admin: devices: phone: #Actor & {
			user:     "admin"
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

	let adminPhone = users.admin.devices.phone
	let bobPhone = users.bob.devices.phone
	let roomJid = "pep-bookmark@muc.\(scenario.domain)"
	let avatarHash = "98f3b12600db9b8ef65f7d44fa8d6e1bd51d61c3"

	steps: [
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-pep-disco"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-pep-disco"
			type:   "result"
			contains: [
				"urn:xmpp:mam:2",
				"urn:xmpp:mam:2#extended",
				"urn:xmpp:fulltext:0",
				"http://jabber.org/protocol/pubsub",
				"http://jabber.org/protocol/pubsub#pep",
				"http://jabber.org/protocol/pubsub#auto-create",
				"http://jabber.org/protocol/pubsub#auto-subscribe",
				"http://jabber.org/protocol/pubsub#access-presence",
				"http://jabber.org/protocol/pubsub#publish",
				"http://jabber.org/protocol/pubsub#retrieve-items",
				"http://jabber.org/protocol/pubsub#persistent-items",
				"http://jabber.org/protocol/pubsub#subscribe",
				"http://jabber.org/protocol/pubsub#filtered-notifications",
				"urn:xmpp:push:0",
				"urn:xmpp:bookmarks:1#compat",
				"urn:xmpp:bookmarks:1#compat-pep",
			]
			absent: [
				"urn:xmpp:pep-vcard-conversion:0",
				"http://jabber.org/protocol/pubsub#config-node-max",
				"eu.siacs.conversations.axolotl.whitelisted",
			]
			elements: [
				#XmlElement & {
					name: "identity"
					ns:   "http://jabber.org/protocol/disco#info"
					attrs: {
						category: "pubsub"
						type:     "pep"
					}
				},
			]
		},
		#SendIq & {
			actor: bobPhone
			type:  "get"
			id:    "cue-cross-user-pep-disco"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#info"
			}
		},
		#ExpectIq & {
			target: bobPhone
			id:     "cue-cross-user-pep-disco"
			type:   "result"
			contains: [
				"http://jabber.org/protocol/pubsub",
				"http://jabber.org/protocol/pubsub#auto-create",
				"http://jabber.org/protocol/pubsub#publish",
				"http://jabber.org/protocol/pubsub#retrieve-items",
				"urn:xmpp:bookmarks:1#compat-pep",
			]
			absent: [
				"jabber:iq:search",
				"urn:xmpp:mam:2",
				"http://jabber.org/protocol/pubsub#config-node-max",
				"eu.siacs.conversations.axolotl.whitelisted",
			]
			elements: [
				#XmlElement & {
					name: "identity"
					ns:   "http://jabber.org/protocol/disco#info"
					attrs: {
						category: "pubsub"
						type:     "pep"
					}
				},
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-private-set"
			payload: #XmlElement & {
				name: "query"
				ns:   "jabber:iq:private"
				children: [
					#XmlElement & {
						name: "storage"
						ns:   "storage:bookmarks"
						children: [
							#XmlElement & {
								name: "conference"
								ns:   "storage:bookmarks"
								attrs: {
									name: "CUE Room"
									jid:  roomJid
								}
							},
						]
					},
				]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-private-set", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-private-get"
			payload: #XmlElement & {
				name: "query"
				ns:   "jabber:iq:private"
				children: [
					#XmlElement & {name: "storage", ns: "storage:bookmarks"},
				]
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-private-get"
			type:     "result"
			contains: ["storage:bookmarks", roomJid]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-vcard-set"
			payload: #XmlElement & {
				name: "vCard"
				ns:   "vcard-temp"
				children: [
					#XmlElement & {name: "FN", ns: "vcard-temp", text: "CUE Admin"},
					#XmlElement & {
						name: "PHOTO"
						ns:   "vcard-temp"
						children: [
							#XmlElement & {name: "TYPE", ns: "vcard-temp", text: "image/png"},
							#XmlElement & {name: "BINVAL", ns: "vcard-temp", text: "Q1VF"},
						]
					},
				]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-vcard-set", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-vcard-get"
			payload: #XmlElement & {name: "vCard", ns: "vcard-temp"}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-vcard-get"
			type:     "result"
			contains: ["vcard-temp", "CUE Admin", "Q1VF"]
		},
		#SendPresence & {
			actor: adminPhone
			to:    bobPhone.jid
			payloads: [
				#XmlElement & {
					name: "x"
					ns:   "vcard-temp:x:update"
					children: [
						#XmlElement & {name: "photo", ns: "vcard-temp:x:update", text: avatarHash},
					]
				},
			]
		},
		#ExpectPresence & {
			target:   bobPhone
			elements: [#XmlElement & {name: "photo", ns: "vcard-temp:x:update", text: avatarHash}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-block-get-empty"
			payload: #XmlElement & {name: "blocklist", ns: "urn:xmpp:blocking"}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-block-get-empty"
			type:     "result"
			contains: ["urn:xmpp:blocking"]
			absent:   ["spammer@localhost"]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-block-set"
			payload: #XmlElement & {
				name: "block"
				ns:   "urn:xmpp:blocking"
				children: [
					#XmlElement & {
						name:  "item"
						ns:    "urn:xmpp:blocking"
						attrs: jid: "spammer@\(scenario.domain)"
					},
				]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-block-set", type: "result"},
		#ExpectIq & {
			target:   adminPhone
			type:     "set"
			contains: ["urn:xmpp:blocking", "spammer@localhost"]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-block-get"
			payload: #XmlElement & {name: "blocklist", ns: "urn:xmpp:blocking"}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-block-get"
			type:     "result"
			contains: ["urn:xmpp:blocking", "spammer@localhost"]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-pep-bookmark-publish"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "publish"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:bookmarks:1"
						children: [
							#XmlElement & {
								name:  "item"
								ns:    "http://jabber.org/protocol/pubsub"
								attrs: id: roomJid
								children: [
									#XmlElement & {
										name: "conference"
										ns:   "urn:xmpp:bookmarks:1"
										attrs: {
											name: "Home"
											jid:  roomJid
										}
										children: [
											#XmlElement & {
												name: "extensions"
												ns:   "urn:xmpp:bookmarks:1"
												children: [
													#XmlElement & {
														name: "notify"
														ns:   "urn:xmpp:notification-settings:1"
														children: [
															#XmlElement & {
																name: "on-mention"
																ns:   "urn:xmpp:notification-settings:1"
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
		#ExpectIq & {target: adminPhone, id: "cue-pep-bookmark-publish", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-pep-bookmark-items"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [
					#XmlElement & {
						name:  "items"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: node: "urn:xmpp:bookmarks:1"
					},
				]
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-pep-bookmark-items"
			type:     "result"
			contains: ["urn:xmpp:bookmarks:1", roomJid, "urn:xmpp:notification-settings:1", "on-mention"]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-pep-mood"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "publish"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/mood"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: "current"
						children: [#XmlElement & {
							name: "mood"
							ns:   "http://jabber.org/protocol/mood"
							children: [#XmlElement & {name: "happy", ns: "http://jabber.org/protocol/mood"}]
						}]
					}]
				}]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-pep-mood", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-pep-mood-items"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "items"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/mood"
				}]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-pep-mood-items"
			type:   "result"
			elements: [#XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "items"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/mood"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: "current"
						children: [#XmlElement & {
							name: "mood"
							ns:   "http://jabber.org/protocol/mood"
							children: [#XmlElement & {name: "happy", ns: "http://jabber.org/protocol/mood"}]
						}]
					}]
				}]
			}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-pep-activity"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "publish"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/activity"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: "current"
						children: [#XmlElement & {
							name: "activity"
							ns:   "http://jabber.org/protocol/activity"
							children: [#XmlElement & {name: "working", ns: "http://jabber.org/protocol/activity"}]
						}]
					}]
				}]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-pep-activity", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-pep-activity-items"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "items"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/activity"
				}]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-pep-activity-items"
			type:   "result"
			elements: [#XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "items"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/activity"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: "current"
						children: [#XmlElement & {
							name: "activity"
							ns:   "http://jabber.org/protocol/activity"
							children: [#XmlElement & {name: "working", ns: "http://jabber.org/protocol/activity"}]
						}]
					}]
				}]
			}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-pep-tune"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "publish"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/tune"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: "current"
						children: [#XmlElement & {
							name: "tune"
							ns:   "http://jabber.org/protocol/tune"
							children: [#XmlElement & {name: "title", ns: "http://jabber.org/protocol/tune", text: "CUE Song"}]
						}]
					}]
				}]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-pep-tune", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-pep-tune-items"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "items"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/tune"
				}]
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-pep-tune-items"
			type:   "result"
			elements: [#XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "items"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "http://jabber.org/protocol/tune"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: "current"
						children: [#XmlElement & {
							name: "tune"
							ns:   "http://jabber.org/protocol/tune"
							children: [#XmlElement & {name: "title", ns: "http://jabber.org/protocol/tune", text: "CUE Song"}]
						}]
					}]
				}]
			}]
		},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-pep-avatar-data"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "publish"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "urn:xmpp:avatar:data"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: avatarHash
						children: [#XmlElement & {name: "data", ns: "urn:xmpp:avatar:data", text: "Q1VF"}]
					}]
				}]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-pep-avatar-data", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "set"
			id:    "cue-pep-avatar-metadata"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "publish"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "urn:xmpp:avatar:metadata"
					children: [#XmlElement & {
						name:  "item"
						ns:    "http://jabber.org/protocol/pubsub"
						attrs: id: avatarHash
						children: [#XmlElement & {
							name: "metadata"
							ns:   "urn:xmpp:avatar:metadata"
							children: [#XmlElement & {
								name: "info"
								ns:   "urn:xmpp:avatar:metadata"
								attrs: {
									id:    avatarHash
									bytes: "3"
									type:  "image/png"
								}
							}]
						}]
					}]
				}]
			}
		},
		#ExpectIq & {target: adminPhone, id: "cue-pep-avatar-metadata", type: "result"},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-pep-avatar-disco-items"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "query"
				ns:   "http://jabber.org/protocol/disco#items"
			}
		},
		#ExpectIq & {
			target: adminPhone
			id:     "cue-pep-avatar-disco-items"
			type:   "result"
			elements: [
				#XmlElement & {
					name: "item"
					ns:   "http://jabber.org/protocol/disco#items"
					attrs: {
						jid:  adminPhone.bareJid
						node: "urn:xmpp:avatar:data"
					}
				},
				#XmlElement & {
					name: "item"
					ns:   "http://jabber.org/protocol/disco#items"
					attrs: {
						jid:  adminPhone.bareJid
						node: "urn:xmpp:avatar:metadata"
					}
				},
			]
		},
		#SendIq & {
			actor: adminPhone
			type:  "get"
			id:    "cue-pep-avatar-items"
			to:    adminPhone.bareJid
			payload: #XmlElement & {
				name: "pubsub"
				ns:   "http://jabber.org/protocol/pubsub"
				children: [#XmlElement & {
					name:  "items"
					ns:    "http://jabber.org/protocol/pubsub"
					attrs: node: "urn:xmpp:avatar:metadata"
				}]
			}
		},
		#ExpectIq & {
			target:   adminPhone
			id:       "cue-pep-avatar-items"
			type:     "result"
			contains: ["urn:xmpp:avatar:metadata", avatarHash, "image/png"]
		},
	]
}
