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
		args: ["x", "wrangler", "deploy", "--config", "wrangler.jsonc", "--env", "production"]
		dependsOn: [_t.build]
		inputs: [
			"wrangler.jsonc",
			"dist/**",
		]
	}
	deployPreview: {
		command: "bash"
		args: [
			"-lc",
			"""
			set -euo pipefail

			event_action="$(jq -r '.action // ""' "${GITHUB_EVENT_PATH:-/dev/null}" 2>/dev/null || true)"
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

			marker='<!-- waddle-gui-preview -->'
			worker_name="waddle-gui-pr-${pr_number}"
			run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}"

			upsert_comment() {
			  local body="$1"
			  if [ -z "${GITHUB_REPOSITORY:-}" ]; then
			    return 0
			  fi
			  local existing_id
			  existing_id="$(gh api "repos/${GITHUB_REPOSITORY}/issues/${pr_number}/comments" --paginate | jq -r --arg marker "$marker" '.[] | select(.user.type=="Bot" and (.body | contains($marker))) | .id' | head -n1 || true)"
			  if [ -n "$existing_id" ]; then
			    gh api -X PATCH "repos/${GITHUB_REPOSITORY}/issues/comments/${existing_id}" -f body="$body" >/dev/null
			  else
			    gh api -X POST "repos/${GITHUB_REPOSITORY}/issues/${pr_number}/comments" -f body="$body" >/dev/null
			  fi
			}

			if [ "$event_action" = "closed" ]; then
			  set +e
			  bun x wrangler delete --config wrangler.jsonc --name "$worker_name" --force
			  set -e
			  body="$marker
			GUI preview cleaned up

			Worker: $worker_name
			Status: deleted on PR close."
			  upsert_comment "$body"
			  exit 0
			fi

			log_file="$(mktemp)"
			set +e
			bun x wrangler deploy --config wrangler.jsonc --name "$worker_name" 2>&1 | tee "$log_file"
			deploy_status=${PIPESTATUS[0]}
			set -e

			preview_url="$(grep -Eo 'https://[^ ]+[.]workers[.]dev' "$log_file" | head -n1 || true)"
			if [ "$deploy_status" -eq 0 ]; then
			  body="$marker
			GUI preview deployed

			URL: ${preview_url:-"(deployment URL not reported by command output)"}
			Worker: $worker_name"
			  upsert_comment "$body"
			  exit 0
			fi

			body="$marker
			GUI preview deploy failed

			Worker: $worker_name
			Outcome: failure
			Logs: ${run_url}"
			upsert_comment "$body"
			exit "$deploy_status"
			""",
		]
		dependsOn: [_t.build]
	}
	deletePreview: {
		command: "sh"
		args: [
			"-c",
			"bun x wrangler delete --config wrangler.jsonc --name waddle-gui-pr-${PR_NUMBER:?PR_NUMBER is required} --force",
		]
	}
}
