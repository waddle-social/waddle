package cuenv

#PlaceholderDigest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
#Digest: =~"^sha256:[a-f0-9]{64}$" & !=#PlaceholderDigest
#GitSha: =~"^[a-f0-9]{40}$"

#LinkBoardDigest: #Digest @tag(linkBoardDigest)
#AiChatbotDigest: #Digest @tag(aiChatbotDigest)
#DecisionPollsDigest: #Digest @tag(decisionPollsDigest)
#GithubDigest: #Digest @tag(githubDigest)
#StargateQuotesDigest: #Digest @tag(stargateQuotesDigest)
#ServerImageDigest: #Digest @tag(serverImageDigest)

#GitHubActionsRoom: "github-actions@muc.waddle.social"

#PublishedExtensionModules: [
	{
		name:      "link-board"
		registry:  "ghcr.io/waddle-social/waddle/extensions/link-board"
		digest:    #LinkBoardDigest
		namespace: "urn:waddle:link-board:1"
		config: {}
		capabilityGrants: [
			"message.enrich",
			"launch",
			"pubsub.publish",
			"ui.declarative",
		]
	},
	{
		name:      "ai-chatbot"
		registry:  "ghcr.io/waddle-social/waddle/extensions/ai-chatbot"
		digest:    #AiChatbotDigest
		namespace: "urn:waddle:ai-chatbot:1"
		config: {
			endpoint: "https://openrouter.ai/api/v1/chat/completions"
			model:    "openrouter/free"
		}
		configSecretFiles: api_key: "/var/run/secrets/waddle-ai/api_key"
		capabilityGrants: [
			"message.enrich",
			"host.mam.read",
			"host.members.read",
			"host.presence.read",
			"host.roster.read",
			"host.channels.read",
			"host.spaces.read",
			"host.message.send",
			"outbound.http.request",
			"commands",
		]
		allowedHttpOrigins: ["https://openrouter.ai"]
	},
	{
		name:      "decision-polls"
		registry:  "ghcr.io/waddle-social/waddle/extensions/decision-polls"
		digest:    #DecisionPollsDigest
		namespace: "urn:waddle:decision-polls:1"
		config: {}
		capabilityGrants: [
			"message.enrich",
			"commands",
			"launch",
			"pubsub.publish",
			"host.message.send",
			"ui.declarative",
		]
	},
	{
		name:      "github"
		registry:  "ghcr.io/waddle-social/waddle/extensions/github"
		digest:    #GithubDigest
		namespace: "urn:waddle:web-integration:1"
		config: admins: ["rawkode@waddle.social"]
		providerRoomGrants: [#GitHubActionsRoom]
		capabilityGrants: [
			"message.enrich",
			"host.message.send",
			"commands",
			"pubsub.publish",
		]
	},
	{
		name:      "stargate-quotes"
		registry:  "ghcr.io/waddle-social/waddle/extensions/stargate-quotes"
		digest:    #StargateQuotesDigest
		namespace: "urn:waddle:stargate-quotes:1"
		config: {}
		capabilityGrants: [
			"commands",
			"host.message.send",
			"message.enrich",
		]
	},
]

#CheckedInGitOpsValues: {
	extensions: {
		enabled: false
		modules: []
		...
	}
	extraSecretRefs: ["waddle-runtime-secrets", "waddle-clustering-keypool", "waddle-livekit-config"]
	secret: {
		runtimeSecretName: "waddle-runtime-secrets"
		...
	}
	...
}

#PublishedValues: {
	image: {
		digest: #ServerImageDigest
		...
	}
	extensions: {
		enabled: true
		modules:  #PublishedExtensionModules
		...
	}
	...
}

#RuntimeExternalSecret: {
	apiVersion: "external-secrets.io/v1beta1"
	kind:       "ExternalSecret"
	metadata: {
		name: "waddle-runtime-secrets"
		...
	}
	spec: {
		data: [
			{secretKey: "WADDLE_SESSION_KEY", remoteRef: {key: "server-runtime-production", property: "session-key"}},
			{secretKey: "WADDLE_OCCUPANT_ID_SECRET", remoteRef: {key: "server-runtime-production", property: "occupant-id-secret"}},
			{secretKey: "WADDLE_S3_ACCESS_KEY_ID", remoteRef: {key: "server-runtime-production", property: "r2-access-key-id"}},
			{secretKey: "WADDLE_S3_SECRET_ACCESS_KEY", remoteRef: {key: "server-runtime-production", property: "r2-secret-access-key"}},
			{secretKey: "WADDLE_SPICEDB_PRESHARED_KEY", remoteRef: {key: "server-runtime-production", property: "spicedb-preshared-key"}},
			{secretKey: "WADDLE_PROVIDER_GITHUB_WEBHOOK_SECRET", remoteRef: {key: "server-runtime-production", property: "github-app-webhook-secret"}},
		]
		...
	}
	...
}

#OpenRouterExternalSecret: {
	apiVersion: "external-secrets.io/v1beta1"
	kind:       "ExternalSecret"
	metadata: {
		name: "waddle-openrouter"
		...
	}
	spec: {
		data: [
			{secretKey: "apiKey", remoteRef: {key: "server-runtime-production", property: "openrouter-api-key"}},
		]
		...
	}
	...
}

#SpiceDbExternalSecret: {
	apiVersion: "external-secrets.io/v1beta1"
	kind:       "ExternalSecret"
	metadata: {
		name: "spicedb-config"
		...
	}
	spec: {
		data: [
			{secretKey: "postgres_username", remoteRef: {key: "spicedb", property: "postgres-username"}},
			{secretKey: "postgres_password", remoteRef: {key: "spicedb", property: "postgres-password"}},
			{secretKey: "preshared_key", remoteRef: {key: "server-runtime-production", property: "spicedb-preshared-key"}},
		]
		...
	}
	...
}

#ExternalSecretNoDefaultPassword: {
	kind: "ExternalSecret"
	spec: {
		data: [...{
			remoteRef: {
				property: !="password"
				...
			}
			...
		}]
		...
	}
	...
}
