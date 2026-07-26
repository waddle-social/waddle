package contributors

import "github.com/cuenv/cuenv/schema"

// #Nix mirrors cuenv's Nix contributor while pinning the GitHub Action to the
// immutable revision behind v3.21.8. Keep the script fallback aligned with the
// upstream contributor when upgrading cuenv.
#Nix: schema.#Contributor & {
	id: "nix"
	tasks: [{
		id:       "nix.install"
		label:    "Install Determinate Nix"
		priority: 2
		script:   "curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none"
		provider: github: {
			uses: "DeterminateSystems/determinate-nix-action@d96678350ffd6a456235832eb11e1c491589b7bb"
			with: "extra-conf": "accept-flake-config = true"
		}
	}]
}

// #Hestia mirrors cuenv 0.55's Hestia contributor without enabling its
// repository-wide generated workflow. Waddle owns that workflow separately so
// its privileged Nix installer can also be pinned to an immutable revision.
#Hestia: schema.#Contributor & {
	id: "hestia"
	tasks: [{
		id:       "hestia.setup"
		label:    "Setup Hestia Nix Cache"
		priority: 4
		dependsOn: ["nix.install"]
		provider: github: {
			uses: "Mic92/hestia@fb239a2f72d4b6e26eec5425f289dea23b27a527"
			with: {
				version:                 "v2.0.0"
				"upstream-cache-filter": "true"
				"drain-timeout":         "900"
			}
		}
	}]
}
