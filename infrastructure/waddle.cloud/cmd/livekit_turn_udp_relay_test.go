package cmd

import (
	"os/exec"
	"strings"
	"testing"
)

// livekitChartPath is the chart directory relative to this package.
const livekitChartPath = "../charts/livekit-sfu"

// livekitBaselineValues is a known-good production-equivalent value set
// (mirrors the `helm lint` invocation in env.cue): TURN enabled over UDP
// with the NodePort exposed and external-IP advertisement on. Guards must
// not reject this healthy configuration.
var livekitBaselineValues = []string{
	"apiKeys.existingSecret=livekit-sfu-api-keys",
	"turn.secretName=turn-waddle-social-tls",
	"livekit.turn.enabled=true",
	"livekit.turn.domain=turn.waddle.social",
	"nodePorts.turnUdp.enabled=true",
}

// renderLivekitChart runs `helm template` against the livekit-sfu chart
// with the baseline values plus any overrides, returning combined output.
func renderLivekitChart(t *testing.T, overrides ...string) (string, error) {
	t.Helper()
	helm, err := exec.LookPath("helm")
	if err != nil {
		t.Skip("helm not on PATH; skipping chart-render test")
	}

	args := []string{"template", "livekit-sfu", livekitChartPath}
	for _, v := range append(append([]string{}, livekitBaselineValues...), overrides...) {
		args = append(args, "--set", v)
	}

	out, err := exec.Command(helm, args...).CombinedOutput()
	return string(out), err
}

// A TURN/UDP listener that is configured but not exposed as a NodePort is
// the silent "stuck on TCP/443" trap: LiveKit advertises a udp relay
// candidate clients cannot reach. The chart must reject that combination.
func TestChartRejectsTurnUDPWithoutNodePort(t *testing.T) {
	out, err := renderLivekitChart(t, "nodePorts.turnUdp.enabled=false")
	if err == nil {
		t.Fatalf("chart rendered with TURN/UDP listener but no NodePort; expected failure.\n%s", out)
	}
	if !strings.Contains(out, "nodePorts.turnUdp.enabled") {
		t.Fatalf("failure message should name nodePorts.turnUdp.enabled; got:\n%s", out)
	}
}

// NodePort-exposed media is pointless if LiveKit advertises its in-cluster
// pod IP: clients (and the TURN relay path) need the node's external IP in
// ICE candidates. The chart must reject NodePorts with use_external_ip off.
func TestChartRejectsNodePortMediaWithoutExternalIP(t *testing.T) {
	out, err := renderLivekitChart(t, "livekit.rtc.use_external_ip=false")
	if err == nil {
		t.Fatalf("chart rendered NodePort media with use_external_ip=false; expected failure.\n%s", out)
	}
	if !strings.Contains(out, "use_external_ip") {
		t.Fatalf("failure message should name use_external_ip; got:\n%s", out)
	}
}
