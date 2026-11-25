package rusttests

import (
	namespace "github.com/waddle-social/waddle:namespace"
	server "github.com/waddle-social/waddle/server:cuenv"
)

// Each task's string inputs are relative to its project. ModuleFiles already
// use repository paths. The namespace pipeline delegates to the root package.
queueTasks: {
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
