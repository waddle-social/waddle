package cmd

import (
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

type coturnConfigMap struct {
	Data map[string]string `yaml:"data"`
}

type coturnExternalSecret struct {
	Spec coturnExternalSecretSpec `yaml:"spec"`
}

type coturnExternalSecretSpec struct {
	Target coturnSecretTarget `yaml:"target"`
}

type coturnSecretTarget struct {
	Template coturnSecretTemplate `yaml:"template"`
}

type coturnSecretTemplate struct {
	Data map[string]string `yaml:"data"`
}

type coturnRTCConfig struct {
	Servers []coturnSignalServer `yaml:"turn_servers"`
}

type coturnSignalServer struct {
	Host, Protocol, Secret string
	Port, TTL              int
}

type coturnNetworkPolicy struct {
	Spec coturnNetworkPolicySpec `yaml:"spec"`
}

type coturnNetworkPolicySpec struct {
	Ingress []coturnTrafficRule `yaml:"ingress"`
	Egress  []coturnTrafficRule `yaml:"egress"`
}

type coturnTrafficRule struct {
	ToPorts []coturnPortGroup `yaml:"toPorts"`
}

type coturnPortGroup struct {
	Ports []coturnPolicyPort `yaml:"ports"`
}

type coturnPolicyPort struct {
	Port, Protocol string
}

func TestProductionUsesSharedTURN(t *testing.T) {
	hr := readYAML[livekitHelmRelease](t, gitOpsPath("livekit-sfu", "helmrelease.yaml"))
	if hr.Spec.Values.Livekit.Turn.Enabled {
		t.Fatal("production must use shared coturn, not embedded TURN")
	}
	if hr.Spec.Values.SharedTURN.ConfigSecretName != "livekit-turn-config" {
		t.Fatal("production must mount the shared TURN server config from its credential Secret")
	}
}

func TestSharedTURNRetainsUDPNodePortAndSelectsCoturn(t *testing.T) {
	out, err := renderLivekitTemplate(t, "templates/turn-udp-nodeport-service.yaml",
		"livekit.turn.enabled=false", "sharedTURN.configSecretName=livekit-turn-config")
	if err != nil {
		t.Fatalf("render shared TURN: %v\n%s", err, out)
	}
	var service k8sService
	if err := yaml.Unmarshal([]byte(out), &service); err != nil {
		t.Fatal(err)
	}
	if service.Metadata.Name != "livekit-sfu-turn-udp" || len(service.Spec.Ports) != 1 || service.Spec.Ports[0].NodePort != 30478 {
		t.Fatal("the existing Helm-owned UDP NodePort must survive the coturn migration")
	}
	if service.Spec.Selector["app.kubernetes.io/name"] != "coturn" || service.Spec.Selector["app.kubernetes.io/instance"] != "coturn" {
		t.Fatal("the existing TURN Service must select the separate coturn pod")
	}
}

func TestSharedTURNRejectsEmbeddedRelayAndMissingPrivateStorage(t *testing.T) {
	for _, override := range []string{"livekit.turn.enabled=true", "tmpDir.enabled=false", "tmpDir.mountPath=/elsewhere"} {
		t.Run(override, func(t *testing.T) {
			out, err := renderLivekitChart(t, "livekit.turn.enabled=false", "sharedTURN.configSecretName=livekit-turn-config", override)
			if err == nil || !strings.Contains(out, "sharedTURN.configSecretName") {
				t.Fatalf("unsafe shared TURN configuration was accepted: %v\n%s", err, out)
			}
		})
	}
}

func TestSharedTURNCredentialsStayInSecretVolumes(t *testing.T) {
	out, err := renderLivekitTemplate(t, "templates/configmap.yaml",
		"livekit.turn.enabled=false", "sharedTURN.configSecretName=livekit-turn-config")
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out, "turn_servers:") || strings.Contains(out, "secret:") {
		t.Fatal("shared TURN credentials must not enter the rendered ConfigMap")
	}
	out, err = renderLivekitTemplate(t, "templates/deployment.yaml",
		"livekit.turn.enabled=false", "sharedTURN.configSecretName=livekit-turn-config")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "secretName: livekit-turn-config") || !strings.Contains(out, "umask 077") || !strings.Contains(out, "--config /tmp/livekit.yaml") {
		t.Fatal("SFU must compose its shared TURN configuration privately from a mounted Secret")
	}
}

func TestSharedTURNUsesTheExistingRESTSecretForBothTransports(t *testing.T) {
	secret := readYAML[coturnExternalSecret](t, gitOpsPath("livekit-sfu", "coturn-external-secret.yaml"))
	fragment := secret.Spec.Target.Template.Data["turn-servers.yaml"]
	if strings.Count(fragment, "{{ .sharedSecret | quote }}") != 2 {
		t.Fatal("both signal-advertised transports must use the same REST secret")
	}
	var rtc coturnRTCConfig
	if err := yaml.Unmarshal([]byte(strings.ReplaceAll(fragment, "{{ .sharedSecret | quote }}", "\"fixture-secret\"")), &rtc); err != nil {
		t.Fatal(err)
	}
	if len(rtc.Servers) != 2 {
		t.Fatal("expected UDP and TLS signal ICE servers")
	}
	for _, server := range rtc.Servers {
		assertCoturnSignalCredentials(t, server)
		assertCoturnSignalTransport(t, server)
	}
}

func TestCoturnKeepsRelayPeersAndPortsNarrow(t *testing.T) {
	config := readYAML[coturnConfigMap](t, gitOpsPath("livekit-sfu", "coturn-config.yaml"))
	base := config.Data["turnserver.conf"]
	for _, required := range []string{"denied-peer-ip=0.0.0.0-255.255.255.255", "denied-peer-ip=::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", "no-tcp-relay", "no-multicast-peers"} {
		if !strings.Contains(base, required) {
			t.Fatalf("coturn is missing peer restriction %s", required)
		}
	}
	policy := readYAML[coturnNetworkPolicy](t, gitOpsPath("livekit-sfu", "coturn-networkpolicy.yaml"))
	if len(policy.Spec.Egress) == 0 {
		t.Fatal("coturn requires an explicit SFU media destination policy")
	}
	for _, rule := range policy.Spec.Egress {
		if len(rule.ToPorts) == 0 {
			t.Fatal("coturn egress may not permit arbitrary destination ports")
		}
		for _, group := range rule.ToPorts {
			assertCoturnMediaPorts(t, group)
		}
	}
}

func assertCoturnMediaPorts(t *testing.T, group coturnPortGroup) {
	t.Helper()
	for _, port := range group.Ports {
		if port.Port != "30882" || port.Protocol != "UDP" {
			t.Fatalf("coturn may only relay to the SFU UDP media port, got %s/%s", port.Port, port.Protocol)
		}
	}
}

func assertCoturnSignalCredentials(t *testing.T, server coturnSignalServer) {
	t.Helper()
	if server.Host != "turn.waddle.social" || server.Secret != "fixture-secret" || server.TTL != turnCredentialTTLSeconds {
		t.Fatal("signal credentials must match the XEP-0215 host and lifetime")
	}
}

func assertCoturnSignalTransport(t *testing.T, server coturnSignalServer) {
	t.Helper()
	expectedPorts := map[string]int{"udp": 30478, "tls": 443}
	expectedPort, knownTransport := expectedPorts[server.Protocol]
	if !knownTransport || expectedPort != server.Port {
		t.Fatalf("unexpected signal TURN endpoint: %s:%d/%s", server.Host, server.Port, server.Protocol)
	}
}
