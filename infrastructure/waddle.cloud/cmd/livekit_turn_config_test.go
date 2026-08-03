package cmd

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

// turnCredentialTTLSeconds is the validity window the chart must set for
// the TURN credentials livekit-server mints itself: v1.12+ embeds the
// expiry in the username and enforces it on new allocations (upstream
// default 300), so a short window breaks mid-call allocations such as an
// ICE restart more than 5 minutes into a call. 3600 also matches the
// window waddle-server uses for the credentials it mints
// (`LIVEKIT_TURN_TTL_SECONDS`, server/crates/waddle-sfu/src/config.rs);
// the embedded TURN cannot validate those at all (#1447) — this key does
// not affect that.
const turnCredentialTTLSeconds = 3600

// k8sConfigMap is the rendered ConfigMap shape we assert on.
type k8sConfigMap struct {
	Data map[string]string `yaml:"data"`
}

// chartValues captures the TURN posture knobs of the chart defaults.
type chartValues struct {
	Livekit struct {
		Turn struct {
			Enabled bool   `yaml:"enabled"`
			Domain  string `yaml:"domain"`
			TTL     *int   `yaml:"ttl_seconds"`
			UDPPort int    `yaml:"udp_port"`
		} `yaml:"turn"`
	} `yaml:"livekit"`
	NodePorts struct {
		TurnUDP struct {
			Enabled bool `yaml:"enabled"`
		} `yaml:"turnUdp"`
	} `yaml:"nodePorts"`
}

// renderLivekitConfigYAML renders only the ConfigMap and returns the parsed
// livekit config.yaml payload.
func renderLivekitConfigYAML(t *testing.T, overrides ...string) map[string]any {
	t.Helper()
	out, err := renderLivekitTemplate(t, "templates/configmap.yaml", overrides...)
	if err != nil {
		t.Fatalf("render configmap: %v\n%s", err, out)
	}

	var cm k8sConfigMap
	if err := yaml.Unmarshal([]byte(out), &cm); err != nil {
		t.Fatalf("unmarshal configmap: %v\n%s", err, out)
	}

	var config map[string]any
	if err := yaml.Unmarshal([]byte(cm.Data["config.yaml"]), &config); err != nil {
		t.Fatalf("unmarshal config.yaml: %v\n%s", err, cm.Data["config.yaml"])
	}
	return config
}

// turnConfig extracts the turn section of a rendered livekit config.
func turnConfig(t *testing.T, config map[string]any) map[string]any {
	t.Helper()
	turn, ok := config["turn"].(map[string]any)
	if !ok {
		t.Fatalf("rendered config.yaml has no turn section: %v", config)
	}
	return turn
}

// renderLivekitChartWithValuesFile renders the chart with the baseline
// values plus a caller-supplied values file, returning combined output.
// Unlike --set overrides, a values file can carry an explicit `null`
// while keeping the key present — a distinct shape the guards must
// also reject.
func renderLivekitChartWithValuesFile(t *testing.T, valuesYAML string) (string, error) {
	t.Helper()
	helm, err := exec.LookPath("helm")
	if err != nil {
		t.Skip("helm not on PATH; skipping chart-render test")
	}

	valuesPath := filepath.Join(t.TempDir(), "values.yaml")
	if err := os.WriteFile(valuesPath, []byte(valuesYAML), 0o600); err != nil {
		t.Fatalf("write values file: %v", err)
	}

	args := []string{"template", "livekit-sfu", livekitChartPath, "--values", valuesPath}
	for _, v := range livekitBaselineValues {
		args = append(args, "--set", v)
	}

	out, err := exec.Command(helm, args...).CombinedOutput()
	return string(out), err
}

// livekit-server v1.12+ authenticates TURN with time-limited credentials
// it mints itself, valid for `turn.ttl_seconds` (default 300). The chart
// must carry an explicit value or mid-call allocations break five minutes
// in after the upgrade (see turnCredentialTTLSeconds).
func TestChartRejectsTurnWithoutCredentialTTL(t *testing.T) {
	out, err := renderLivekitChart(t, "livekit.turn.ttl_seconds=null")
	if err == nil {
		t.Fatalf("chart rendered TURN without ttl_seconds; expected failure.\n%s", out)
	}
	if !strings.Contains(out, "ttl_seconds") {
		t.Fatalf("failure message should name ttl_seconds; got:\n%s", out)
	}
}

// livekit-server v1.12+ denies TURN relay to restricted peers (loopback,
// link-local, private, multicast, unspecified) unless allowed. The chart
// must force that policy to be an explicit decision rather than an
// inherited default.
func TestChartRejectsTurnWithoutRestrictedPeerCIDRPolicy(t *testing.T) {
	out, err := renderLivekitChart(t, "livekit.turn.allow_restricted_peer_cidrs=null")
	if err == nil {
		t.Fatalf("chart rendered TURN without allow_restricted_peer_cidrs; expected failure.\n%s", out)
	}
	if !strings.Contains(out, "allow_restricted_peer_cidrs") {
		t.Fatalf("failure message should name allow_restricted_peer_cidrs; got:\n%s", out)
	}
}

// A values file carrying an explicit `allow_restricted_peer_cidrs: null`
// is merged by Helm's null-deletes-the-key semantics, so the hasKey guard
// fires for it just like for an absent key. This pins that behavior —
// if Helm ever kept the nil-valued key, the guard would silently stop
// covering this shape.
func TestChartRejectsTurnWithNullRestrictedPeerCIDRPolicy(t *testing.T) {
	out, err := renderLivekitChartWithValuesFile(t, "livekit:\n  turn:\n    allow_restricted_peer_cidrs: null\n")
	if err == nil {
		t.Fatalf("chart rendered TURN with a null allow_restricted_peer_cidrs; expected failure.\n%s", out)
	}
	if !strings.Contains(out, "allow_restricted_peer_cidrs") {
		t.Fatalf("failure message should name allow_restricted_peer_cidrs; got:\n%s", out)
	}
}

// The rendered config must carry the TURN auth keys the v1.12+ server
// requires, so the upgrade (#1531) needs only an image bump. The deployed
// v1.11.0 predates the keys but runs `--disable-strict-config`
// (templates/deployment.yaml), so it ignores them.
func TestConfigMapCarriesTurnAuthKeys(t *testing.T) {
	config := renderLivekitConfigYAML(t)
	turn := turnConfig(t, config)

	ttl, ok := turn["ttl_seconds"].(int)
	if !ok || ttl != turnCredentialTTLSeconds {
		t.Fatalf("turn.ttl_seconds = %v, want %d", turn["ttl_seconds"], turnCredentialTTLSeconds)
	}
	if _, ok := turn["allow_restricted_peer_cidrs"]; !ok {
		t.Fatalf("turn.allow_restricted_peer_cidrs missing from rendered config: %v", turn)
	}
}

// The production HelmRelease must state the TURN auth posture explicitly
// rather than silently inheriting chart defaults: the restricted-peer
// relay policy is a deliberate per-deployment security decision, and the
// credential window must survive a chart-default change.
func TestProductionHelmReleaseStatesTurnAuthPosture(t *testing.T) {
	hr := readYAML[livekitHelmRelease](t, gitOpsPath("livekit-sfu", "helmrelease.yaml"))
	turn := hr.Spec.Values.Livekit.Turn

	if turn.TTL == nil || *turn.TTL != turnCredentialTTLSeconds {
		t.Fatalf("production livekit.turn.ttl_seconds = %v, want explicit %d", turn.TTL, turnCredentialTTLSeconds)
	}
	if turn.AllowRestrictedPeerCIDRs == nil {
		t.Fatal("production livekit.turn.allow_restricted_peer_cidrs must be set explicitly (empty list is the deliberate choice for our public-candidate-only topology)")
	}
	if len(*turn.AllowRestrictedPeerCIDRs) != 0 {
		t.Fatalf("production livekit.turn.allow_restricted_peer_cidrs = %v; widening it lets any TURN-authenticated client relay UDP into that range — change this test only with a topology rationale", *turn.AllowRestrictedPeerCIDRs)
	}
}

// Production has run TURN-enabled since the relay went live, while the
// chart default said `false` — drift that already misled one
// investigation. The chart defaults must state the production posture.
func TestChartDefaultsMatchProductionTurnPosture(t *testing.T) {
	values := readYAML[chartValues](t, "../charts/livekit-sfu/values.yaml")
	turn := values.Livekit.Turn

	if !turn.Enabled {
		t.Fatal("values.yaml livekit.turn.enabled must default to true (production posture)")
	}
	if turn.Domain != "turn.waddle.social" {
		t.Fatalf("values.yaml livekit.turn.domain = %q, want turn.waddle.social", turn.Domain)
	}
	if turn.TTL == nil || *turn.TTL != turnCredentialTTLSeconds {
		t.Fatalf("values.yaml livekit.turn.ttl_seconds = %v, want %d (see turnCredentialTTLSeconds)", turn.TTL, turnCredentialTTLSeconds)
	}
	if turn.UDPPort != turnUDPPort {
		t.Fatalf("values.yaml livekit.turn.udp_port = %d, want %d", turn.UDPPort, turnUDPPort)
	}
	if !values.NodePorts.TurnUDP.Enabled {
		t.Fatal("values.yaml nodePorts.turnUdp.enabled must default to true (production posture)")
	}
}
