package talos

import (
	"strings"
	"testing"
)

func TestBuildCloudInitOmitsDebugOutputByDefault(t *testing.T) {
	cloudInit := BuildCloudInit(PivotParams{
		TalosVersion:   "v1.12.0",
		TalosSchematic: "schematic-id",
	})

	if strings.Contains(cloudInit, "cloud-init-output.log") {
		t.Fatalf("cloud-init unexpectedly contains debug output block: %s", cloudInit)
	}
	if strings.Contains(cloudInit, "waddle-cloud-pivot.log") {
		t.Fatalf("cloud-init unexpectedly contains debug pivot log wiring: %s", cloudInit)
	}
}

func TestBuildCloudInitIncludesDebugOutputWhenEnabled(t *testing.T) {
	cloudInit := BuildCloudInit(PivotParams{
		TalosVersion:   "v1.12.0",
		TalosSchematic: "schematic-id",
		DebugProvision: true,
	})

	for _, expected := range []string{
		`output:`,
		`/var/log/cloud-init-output.log`,
		`/dev/console`,
		`/var/log/waddle-cloud-pivot.log`,
		`/run/waddle-cloud-pivot-step`,
		`trap 'pivot_failed' ERR`,
		`set -x`,
	} {
		if !strings.Contains(cloudInit, expected) {
			t.Fatalf("cloud-init missing %q:\n%s", expected, cloudInit)
		}
	}
}

func TestBuildPivotScriptIncludesDebugStepMarkers(t *testing.T) {
	script := BuildPivotScript(PivotParams{
		TalosVersion:   "v1.12.0",
		TalosSchematic: "schematic-id",
		DebugProvision: true,
	})

	for _, expected := range []string{
		`log_step "install dependencies"`,
		`log_step "download talos image"`,
		`log_step "clear EFI boot variables"`,
		`log_step "wipe data disk signatures"`,
		`log_step "write Talos image to OS disk"`,
		`log_step "fix GPT backup header"`,
		`log_step "create EFI boot entry"`,
		`log_step "reboot into Talos maintenance mode"`,
		`/var/log/waddle-cloud-pivot.log`,
		`pivot failed step=`,
	} {
		if !strings.Contains(script, expected) {
			t.Fatalf("pivot script missing %q:\n%s", expected, script)
		}
	}
}
