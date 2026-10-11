package cmd

import (
	"os/exec"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

// turnUDPPort is the production NodePort for the embedded TURN/UDP relay
// listener, kept in lockstep across the chart, gitops, and networkpolicy.
const turnUDPPort = 30478

// livekitHelmRelease captures the TURN/RTC reachability knobs of the
// production Flux HelmRelease values.
type livekitHelmRelease struct {
	Spec struct {
		Values struct {
			Livekit struct {
				RTC struct {
					UseExternalIP bool `yaml:"use_external_ip"`
				} `yaml:"rtc"`
				Turn struct {
					Enabled                  bool      `yaml:"enabled"`
					UDPPort                  int       `yaml:"udp_port"`
					TTL                      *int      `yaml:"ttl_seconds"`
					AllowRestrictedPeerCIDRs *[]string `yaml:"allow_restricted_peer_cidrs"`
				} `yaml:"turn"`
			} `yaml:"livekit"`
			NodePorts struct {
				TurnUDP struct {
					Enabled bool `yaml:"enabled"`
					UDP     int  `yaml:"udp"`
				} `yaml:"turnUdp"`
			} `yaml:"nodePorts"`
		} `yaml:"values"`
	} `yaml:"spec"`
}

// ciliumNetworkPolicy captures the ingress and egress port rules of a
// livekit-stack CiliumNetworkPolicy.
type ciliumNetworkPolicy struct {
	Spec struct {
		Ingress []struct {
			FromEntities []string `yaml:"fromEntities"`
			ToPorts      []struct {
				Ports []struct {
					Port     string `yaml:"port"`
					Protocol string `yaml:"protocol"`
				} `yaml:"ports"`
			} `yaml:"toPorts"`
		} `yaml:"ingress"`
		Egress []ciliumEgressRule `yaml:"egress"`
	} `yaml:"spec"`
}

type ciliumEgressRule struct {
	ToEntities  []string `yaml:"toEntities"`
	ToEndpoints []struct {
		MatchLabels map[string]string `yaml:"matchLabels"`
	} `yaml:"toEndpoints"`
	ToPorts []struct {
		Ports []struct {
			Port     string `yaml:"port"`
			Protocol string `yaml:"protocol"`
		} `yaml:"ports"`
	} `yaml:"toPorts"`
}

func (r ciliumEgressRule) opensPort(port, proto string) bool {
	for _, tp := range r.ToPorts {
		for _, pt := range tp.Ports {
			if pt.Port == port && pt.Protocol == proto {
				return true
			}
		}
	}
	return false
}

// egressOpens reports whether any egress rule allows port/proto out (to
// any target).
func (p ciliumNetworkPolicy) egressOpens(port, proto string) bool {
	for _, rule := range p.Spec.Egress {
		if rule.opensPort(port, proto) {
			return true
		}
	}
	return false
}

// egressOpensToEntity reports whether an egress rule allows port/proto to a
// named Cilium entity (e.g. "world").
func (p ciliumNetworkPolicy) egressOpensToEntity(port, proto, entity string) bool {
	for _, rule := range p.Spec.Egress {
		if !rule.opensPort(port, proto) {
			continue
		}
		for _, e := range rule.ToEntities {
			if e == entity {
				return true
			}
		}
	}
	return false
}

// egressOpensToInstance reports whether an egress rule allows port/proto to
// an in-cluster endpoint selected by app.kubernetes.io/instance.
func (p ciliumNetworkPolicy) egressOpensToInstance(port, proto, instance string) bool {
	for _, rule := range p.Spec.Egress {
		if !rule.opensPort(port, proto) {
			continue
		}
		for _, ep := range rule.ToEndpoints {
			if ep.MatchLabels["app.kubernetes.io/instance"] == instance {
				return true
			}
		}
	}
	return false
}

// k8sService is the rendered NodePort Service shape we assert on.
type k8sService struct {
	Spec struct {
		Type  string `yaml:"type"`
		Ports []struct {
			Name       string `yaml:"name"`
			Port       int    `yaml:"port"`
			TargetPort string `yaml:"targetPort"`
			NodePort   int    `yaml:"nodePort"`
			Protocol   string `yaml:"protocol"`
		} `yaml:"ports"`
	} `yaml:"spec"`
}

// k8sDeployment exposes the rendered container ports.
type k8sDeployment struct {
	Spec struct {
		Template struct {
			Spec struct {
				Containers []struct {
					Ports []struct {
						Name          string `yaml:"name"`
						ContainerPort int    `yaml:"containerPort"`
						Protocol      string `yaml:"protocol"`
					} `yaml:"ports"`
				} `yaml:"containers"`
			} `yaml:"spec"`
		} `yaml:"template"`
	} `yaml:"spec"`
}

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
	return renderLivekitTemplate(t, "", overrides...)
}

// renderLivekitTemplate renders the chart, optionally restricting output
// to a single template via `--show-only`, and returns combined output.
func renderLivekitTemplate(t *testing.T, showOnly string, overrides ...string) (string, error) {
	t.Helper()
	helm, err := exec.LookPath("helm")
	if err != nil {
		t.Skip("helm not on PATH; skipping chart-render test")
	}

	args := []string{"template", "livekit-sfu", livekitChartPath}
	if showOnly != "" {
		args = append(args, "--show-only", showOnly)
	}
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

// With host networking the LiveKit pod binds the node's interface directly,
// so the TURN/UDP listener is reachable on the node IP without a NodePort
// Service. The guard must exempt that configuration, like the
// use_external_ip guard does.
func TestChartAllowsTurnUDPWithoutNodePortOnHostNetwork(t *testing.T) {
	out, err := renderLivekitChart(t, "nodePorts.turnUdp.enabled=false", "podHostNetwork=true")
	if err != nil {
		t.Fatalf("chart should render TURN/UDP without a NodePort under hostNetwork; got error:\n%s", out)
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

// The production Flux HelmRelease must wire the full UDP relay path so
// LiveKit provisions a usable udp relay candidate: TURN enabled over
// udp_port, that port exposed as a matching NodePort, and external-IP
// advertisement on.
func TestProductionHelmReleaseWiresTurnUDPRelay(t *testing.T) {
	hr := readYAML[livekitHelmRelease](t, gitOpsPath("livekit-sfu", "helmrelease.yaml"))
	v := hr.Spec.Values

	if !v.Livekit.Turn.Enabled {
		t.Fatal("livekit.turn.enabled must be true in production")
	}
	if v.Livekit.Turn.UDPPort != turnUDPPort {
		t.Fatalf("livekit.turn.udp_port = %d, want %d", v.Livekit.Turn.UDPPort, turnUDPPort)
	}
	if !v.NodePorts.TurnUDP.Enabled {
		t.Fatal("nodePorts.turnUdp.enabled must be true so the TURN/UDP listener is reachable")
	}
	if v.NodePorts.TurnUDP.UDP != turnUDPPort {
		t.Fatalf("nodePorts.turnUdp.udp = %d, want %d (must match udp_port)", v.NodePorts.TurnUDP.UDP, turnUDPPort)
	}
	if !v.Livekit.RTC.UseExternalIP {
		t.Fatal("livekit.rtc.use_external_ip must be true so LiveKit advertises a reachable external IP")
	}
}

// The data plane must admit the TURN/UDP relay port from the public
// internet; a NodePort without this allow would render the relay
// unreachable.
func TestNetworkPolicyAdmitsTurnUDPFromWorld(t *testing.T) {
	np := readYAML[ciliumNetworkPolicy](t, gitOpsPath("livekit-sfu", "networkpolicy.yaml"))

	for _, rule := range np.Spec.Ingress {
		if !contains(rule.FromEntities, "world") {
			continue
		}
		for _, tp := range rule.ToPorts {
			for _, p := range tp.Ports {
				if p.Port == "30478" && strings.EqualFold(p.Protocol, "UDP") {
					return
				}
			}
		}
	}
	t.Fatal("networkpolicy has no ingress allow for 30478/UDP from world; TURN/UDP relay would be unreachable")
}

// Rendering the production-equivalent values must yield a turn-udp
// NodePort Service whose external port, nodePort, and target all line up
// on the TURN/UDP port — the wire shape clients dial for a udp relay.
func TestRenderedTurnUDPServiceIsReachable(t *testing.T) {
	out, err := renderLivekitTemplate(t, "templates/turn-udp-nodeport-service.yaml")
	if err != nil {
		t.Fatalf("render turn-udp service: %v\n%s", err, out)
	}

	var svc k8sService
	if err := yaml.Unmarshal([]byte(out), &svc); err != nil {
		t.Fatalf("unmarshal service: %v\n%s", err, out)
	}
	if svc.Spec.Type != "NodePort" {
		t.Fatalf("turn-udp service type = %q, want NodePort", svc.Spec.Type)
	}

	var found bool
	for _, p := range svc.Spec.Ports {
		if p.Name != "turn-udp" {
			continue
		}
		found = true
		if p.Protocol != "UDP" {
			t.Fatalf("turn-udp protocol = %q, want UDP", p.Protocol)
		}
		if p.Port != turnUDPPort || p.NodePort != turnUDPPort {
			t.Fatalf("turn-udp port/nodePort = %d/%d, want %d/%d", p.Port, p.NodePort, turnUDPPort, turnUDPPort)
		}
		if p.TargetPort != "turn-udp" {
			t.Fatalf("turn-udp targetPort = %q, want named port turn-udp", p.TargetPort)
		}
	}
	if !found {
		t.Fatalf("rendered turn-udp service has no turn-udp port:\n%s", out)
	}
}

// The SFU container must declare the turn-udp port so the NodePort's named
// targetPort resolves to a real listener.
func TestRenderedDeploymentExposesTurnUDPContainerPort(t *testing.T) {
	out, err := renderLivekitTemplate(t, "templates/deployment.yaml")
	if err != nil {
		t.Fatalf("render deployment: %v\n%s", err, out)
	}

	var dep k8sDeployment
	if err := yaml.Unmarshal([]byte(out), &dep); err != nil {
		t.Fatalf("unmarshal deployment: %v\n%s", err, out)
	}

	for _, c := range dep.Spec.Template.Spec.Containers {
		for _, p := range c.Ports {
			if p.Name == "turn-udp" && p.ContainerPort == turnUDPPort && p.Protocol == "UDP" {
				return
			}
		}
	}
	t.Fatalf("deployment has no turn-udp/%d/UDP container port:\n%s", turnUDPPort, out)
}

func contains(haystack []string, needle string) bool {
	for _, s := range haystack {
		if s == needle {
			return true
		}
	}
	return false
}
