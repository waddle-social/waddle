package cmd

import (
	"net/url"
	"testing"
)

type webhookReleaseFixture struct{ Spec webhookReleaseSpecFixture }
type webhookReleaseSpecFixture struct{ Values webhookReleaseValuesFixture }
type webhookReleaseValuesFixture struct{ Webhook webhookSettingsFixture }
type webhookSettingsFixture struct {
	Enabled bool
	URLs    []string `yaml:"urls"`
}
type webhookPolicyFixture struct{ Spec webhookPolicySpecFixture }
type webhookPolicySpecFixture struct{ Egress []webhookEgressFixture }
type webhookEgressFixture struct {
	ToEndpoints []webhookEndpointFixture `yaml:"toEndpoints"`
	ToEntities  []string                 `yaml:"toEntities"`
	ToPorts     []webhookPortsFixture    `yaml:"toPorts"`
}
type webhookEndpointFixture struct {
	MatchLabels map[string]string `yaml:"matchLabels"`
}
type webhookPortsFixture struct{ Ports []webhookPortFixture }
type webhookPortFixture struct {
	Port     string
	Protocol string
}

func TestLivekitWebhookUsesPermittedInternalServerEndpoint(t *testing.T) {
	release := readYAML[webhookReleaseFixture](t, gitOpsPath("livekit-sfu", "helmrelease.yaml"))
	webhook := release.Spec.Values.Webhook
	if !webhook.Enabled {
		t.Fatal("LiveKit webhook delivery is disabled")
	}
	if len(webhook.URLs) != 1 {
		t.Fatalf("webhook URLs = %v, want one internal delivery endpoint", webhook.URLs)
	}
	endpoint, err := url.Parse(webhook.URLs[0])
	if err != nil {
		t.Fatalf("parse webhook URL: %v", err)
	}
	if endpoint.Scheme != "http" || endpoint.Hostname() != "waddle-server.waddle.svc.cluster.local" ||
		endpoint.Port() != "3000" || endpoint.Path != "/api/v1/livekit/webhook" {
		t.Fatalf("webhook endpoint %q must reach the signed handler through the internal Service; the public gateway hairpin rejects the SFU", webhook.URLs[0])
	}

	policy := readYAML[webhookPolicyFixture](t, gitOpsPath("livekit-sfu", "networkpolicy.yaml"))
	for _, rule := range policy.Spec.Egress {
		if webhookRuleAllowsTCPPort(rule, endpoint.Port()) {
			assertWebhookRuleScope(t, rule)
			return
		}
	}
	t.Fatalf("no endpoint-scoped policy permits the webhook Service port %s/TCP", endpoint.Port())
}

func webhookRuleAllowsTCPPort(rule webhookEgressFixture, port string) bool {
	for _, ports := range rule.ToPorts {
		for _, candidate := range ports.Ports {
			if candidate.Port == port && candidate.Protocol == "TCP" {
				return true
			}
		}
	}
	return false
}

func assertWebhookRuleScope(t *testing.T, rule webhookEgressFixture) {
	t.Helper()
	if len(rule.ToEntities) != 0 || len(rule.ToEndpoints) != 1 {
		t.Fatal("webhook egress must select only the server endpoints")
	}
	labels := rule.ToEndpoints[0].MatchLabels
	for key, want := range map[string]string{
		"k8s:io.kubernetes.pod.namespace": "waddle",
		"k8s:app.kubernetes.io/name":      "waddle-server",
		"k8s:app.kubernetes.io/instance":  "waddle-server",
	} {
		if got := labels[key]; got != want {
			t.Fatalf("webhook egress selector %s = %q, want %q", key, got, want)
		}
	}
	if len(rule.ToPorts) != 1 || len(rule.ToPorts[0].Ports) != 1 {
		t.Fatal("webhook server egress must be limited to the HTTP listener")
	}
}
