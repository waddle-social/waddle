package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "waddle-gui"

let _t = tasks

env: {
	environment: production: {
		CLOUDFLARE_ACCOUNT_ID: schema.#OnePasswordRef & {
			ref: "op://waddle-production/Cloudflare/username"
		}
		CLOUDFLARE_API_TOKEN: schema.#OnePasswordRef & {
			ref: "op://waddle-production/Cloudflare/password"
		}
	}
}

ci: pipelines: {
	default: {
		environment: "production"
		when: {
			branch:        ["main"]
			defaultBranch: true
			manual:        true
		}
		tasks: [_t.deployProduction]
	}
	pullRequest: {
		environment: "production"
		when: {
			pullRequest: true
		}
		tasks: [_t.deployPreview]
	}
}

tasks: {
	lockfileCheck: {
		command: "bash"
		args: [
			"-lc",
			"""
			set -euo pipefail
			forbidden_lockfiles="$(find . -type f | grep -E '/(package-lock[.]json|yarn[.]lock|pnpm-lock[.]yaml)$' | grep -Ev '^./(node_modules|dist)/' || true)"
			if [ -n "$forbidden_lockfiles" ]; then
			  echo "Forbidden lockfiles found in app/gui:"
			  echo "$forbidden_lockfiles"
			  exit 1
			fi
			""",
		]
		inputs: [
			"package.json",
			"bun.lock",
		]
	}
	install: {
		command: "bun"
		args: ["install", "--frozen-lockfile"]
		dependsOn: [_t.lockfileCheck]
		inputs: [
			"package.json",
			"bun.lock",
		]
	}
	build: {
		command: "bun"
		args: ["run", "build"]
		dependsOn: [_t.install]
		inputs: [
			"package.json",
			"bun.lock",
			"index.html",
			"vite.config.ts",
			"tsconfig*.json",
			"src/**",
			"public/**",
		]
		outputs: [
			"dist/**",
		]
	}
	deployProduction: {
		command: "bun"
		args: ["run", "wrangler", "deploy", "--config", "wrangler.jsonc", "--env", "production"]
		dependsOn: [_t.build]
		inputs: [
			"wrangler.jsonc",
			"dist/**",
		]
		outputs: [
			".wrangler/**",
		]
	}
	deployPreview: {
		command: "bash"
		args: [
			"-lc",
			"""
			set -euo pipefail

			event_action="$(jq -r '.action // ""' "${GITHUB_EVENT_PATH:-/dev/null}" 2>/dev/null || true)"
			if [ -z "$event_action" ] && [ -n "${GITHUB_EVENT_PATH:-}" ] && [ -f "${GITHUB_EVENT_PATH}" ]; then
			  event_action="$(grep -Eo '"action"[[:space:]]*:[[:space:]]*"[^"]+"' "${GITHUB_EVENT_PATH}" | head -n1 | cut -d'"' -f4 || true)"
			fi
			pr_number="${PR_NUMBER:-}"
			if [ -z "$pr_number" ]; then
			  pr_number="$(jq -r '.pull_request.number // .number // empty' "${GITHUB_EVENT_PATH:-/dev/null}" 2>/dev/null || true)"
			fi
			if [ -z "$pr_number" ] && [ -n "${GITHUB_REF:-}" ]; then
			  case "$GITHUB_REF" in
			    refs/pull/*/merge|refs/pull/*/head)
			      pr_number="$(echo "$GITHUB_REF" | cut -d/ -f3)"
			      ;;
			  esac
			fi
			if [ -z "$pr_number" ] && [ -n "${GITHUB_REPOSITORY:-}" ] && [ -n "${GITHUB_SHA:-}" ]; then
			  pr_number="$(gh api "repos/${GITHUB_REPOSITORY}/commits/${GITHUB_SHA}/pulls" --jq '.[0].number' 2>/dev/null || true)"
			fi
			if [ -z "$pr_number" ] && [ -n "${GITHUB_EVENT_PATH:-}" ] && [ -f "${GITHUB_EVENT_PATH}" ]; then
			  pr_number="$(grep -Eo '"number"[[:space:]]*:[[:space:]]*[0-9]+' "${GITHUB_EVENT_PATH}" | head -n1 | grep -Eo '[0-9]+' || true)"
			fi
			if [ -z "$pr_number" ]; then
			  pr_number="$(git for-each-ref --format='%(refname)' refs/remotes/pull | awk -F/ '$1=="refs" && $2=="remotes" && $3=="pull" && $4 ~ /^[0-9]+$/ && $5=="merge" { print $4; exit }' || true)"
			fi
			if [ "$pr_number" = "null" ]; then
			  pr_number=""
			fi
			if [ -z "$pr_number" ]; then
			  echo "PR number is required (set PR_NUMBER or run in pull_request context)."
			  exit 2
			fi

			worker_name="waddle-gui-pr-${pr_number}"

			if [ "$event_action" = "closed" ]; then
			  set +e
			  bun run wrangler delete --config wrangler.jsonc --name "$worker_name" --force
			  set -e
			  exit 0
			fi

			set +e
			deploy_output="$(bun run wrangler deploy --config wrangler.jsonc --name "$worker_name" 2>&1)"
			deploy_status=$?
			set -e
			printf '%s\n' "$deploy_output"

			if [ "$deploy_status" -eq 0 ]; then
			  exit 0
			fi

			echo "Preview deployment failed with exit status: $deploy_status"
			exit "$deploy_status"
			""",
		]
		dependsOn: [_t.build]
		outputs: [
			".wrangler/**",
		]
	}
	deletePreview: {
		command: "sh"
		args: [
			"-c",
			"bun run wrangler delete --config wrangler.jsonc --name waddle-gui-pr-${PR_NUMBER:?PR_NUMBER is required} --force",
		]
		outputs: [
			".wrangler/**",
		]
	}
}
