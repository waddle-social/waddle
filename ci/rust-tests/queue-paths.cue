package rusttests

import (
	android "github.com/waddle-social/waddle/apps/android:cuenv"
	chat "github.com/waddle-social/waddle/chat:cuenv"
	cloud "github.com/waddle-social/waddle/infrastructure/waddle.cloud:cuenv"
	colony "github.com/waddle-social/waddle/colony:cuenv"
	design "github.com/waddle-social/waddle/design:cuenv"
	linux "github.com/waddle-social/waddle/apps/linux:cuenv"
	operator "github.com/waddle-social/waddle/operator:cuenv"
	website "github.com/waddle-social/waddle/website:cuenv"
	namespace "github.com/waddle-social/waddle:namespace"
	server "github.com/waddle-social/waddle/server:cuenv"
)

// Each task's string inputs are relative to its project. ModuleFiles already
// use repository paths. Bun project verification also covers local workspace dependencies. The namespace pipeline delegates to the root package.
queueTasks: {
	"waddle-android-pullrequest":  [{prefix: "apps/android/", tasks: android.ci.pipelines.pullRequest.tasks}]
	"waddle-chat-queuechecks":     [{prefix: "chat/", tasks: chat.ci.pipelines.queueChecks.tasks, workspaceInputs: ["design/**"]}]
	"waddle-cloud-pullrequest":    [{prefix: "infrastructure/waddle.cloud/", tasks: cloud.ci.pipelines.pullRequest.tasks}]
	"waddle-colony-pullrequest":   [{prefix: "colony/", tasks: colony.ci.pipelines.pullRequest.tasks, workspaceInputs: ["design/**"]}]
	"waddle-design-pullrequest":   [{prefix: "design/", tasks: design.ci.pipelines.pullRequest.tasks}]
	"waddle-linux-pullrequest":    [{prefix: "apps/linux/", tasks: linux.ci.pipelines.pullRequest.tasks}]
	"waddle-operator-queuechecks": [{prefix: "operator/", tasks: operator.ci.pipelines.queueChecks.tasks}]
	"waddle-server-pullrequest":   [{prefix: "server/", tasks: server.ci.pipelines.pullRequest.tasks}]
	"waddle-website-pullrequest":  [{prefix: "website/", tasks: website.ci.pipelines.pullRequest.tasks, workspaceInputs: ["design/**"]}]
	"waddle-server-namespacerustchecks": [
		{prefix: "server/", tasks: server.ci.pipelines.namespaceRustChecks.tasks},
		{prefix: "", tasks: [namespace.tasks.test]},
	]
	"waddle-server-queuechecks": [
		{
			prefix: "server/"
			tasks:  server.ci.pipelines.queueChecks.tasks
			// Nix includes these files in serverCheckFiles. The queue
			// validates the whole flake fileset on the combined revision.
			extraInputs: [
				"README.md",
				"capabilities.toml",
				"experiments/native-fixture/**",
				"experiments/grant-fence/Cargo.toml",
				"scripts/check-poison-recovery.py",
				"scripts/check-rust-structure.py",
				"scripts/check-test-postgres-env.py",
				"scripts/check-ingress-typed-entry.py",
				"scripts/ingress-typed-entry-allowlist.txt",
			]
		},
	]
}
