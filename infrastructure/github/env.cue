package cuenv

import (
	"encoding/json"
	"github.com/cuenv/cuenv/schema"
)

let queue = {
	merge_method:                      "SQUASH"
	grouping_strategy:                 "HEADGREEN"
	max_entries_to_build:              1
	min_entries_to_merge:              100
	max_entries_to_merge:              100
	min_entries_to_merge_wait_minutes: 30
	check_response_timeout_minutes:    180
}

schema.#Project & {
	name: "waddle-github"
	runtime: flake: "../.."

	for operation in ["plan", "check", "apply"] {
		tasks: (operation): schema.#Task & {
			command:  "python3"
			args:     ["merge_queue.py", operation, json.Marshal(queue)]
			hermetic: false // Use the operator's authenticated gh session and read GitHub on every run.
		}
	}
}
