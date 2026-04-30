module: "github.com/waddle-social/waddle"
language: {
	version: "v0.14.0"
}
deps: {
	"github.com/cuenv/cuenv@v0": {
		v: "v0.41.2"
	}
}
custom: {
	"github.com/cuenv/cuenv": {
		// The cuenv 0.41.2 tag provides the schema above, but its binary and
		// package metadata still report 0.41.1.
		version: "0.41.1"
	}
}
