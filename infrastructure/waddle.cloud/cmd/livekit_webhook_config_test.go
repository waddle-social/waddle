package cmd

import (
	"net/url"
	"testing"
)

func TestLivekitWebhookUsesPermittedInternalServerEndpoint(t *testing.T) {
	release := readYAML[struct {
		Spec struct {
			Values struct {
				Webhook struct {
					Enabled bool
					URLs    []string `yaml:"urls"`
				}
			}
		}
	}](t, gitOpsPath("livekit-sfu", "helmrelease.yaml"))
	if !release.Spec.Values.Webhook.Enabled {
		t.Fatal("LiveKit webhook delivery is disabled")
	}
	urls := release.Spec.Values.Webhook.URLs
	if len(urls) != 1 {
		t.Fatalf("webhook URLs = %v, want one internal delivery endpoint", urls)
	}
	endpoint, err := url.Parse(urls[0])
	if err != nil {
		t.Fatalf("parse webhook URL: %v", err)
	}
	if endpoint.Scheme != "http" || endpoint.Hostname() != "waddle-server.waddle.svc.cluster.local" ||
		endpoint.Port() != "3000" || endpoint.Path != "/api/v1/livekit/webhook" {
		t.Fatalf("webhook endpoint %q must reach the signed handler through the internal Service; the public gateway hairpin rejects the SFU", urls[0])
	}

	policy := readYAML[struct {
		Spec struct {
			Egress []struct {
				ToEndpoints []struct {
					MatchLabels map[string]string `yaml:"matchLabels"`
				} `yaml:"toEndpoints"`
				ToEntities []string `yaml:"toEntities"`
				ToPorts    []struct {
					Ports []struct {
						Port     string
						Protocol string
					}
				} `yaml:"toPorts"`
			}
		}
	}](t, gitOpsPath("livekit-sfu", "networkpolicy.yaml"))

	for _, rule := range policy.Spec.Egress {
		for _, ports := range rule.ToPorts {
			for _, port := range ports.Ports {
				if port.Port != endpoint.Port() || port.Protocol != "TCP" {
					continue
				}
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
				if len(ports.Ports) != 1 {
					t.Fatal("webhook server egress must be limited to the HTTP listener")
				}
				return
			}
		}
	}
	t.Fatalf("no endpoint-scoped policy permits the webhook Service port %s/TCP", endpoint.Port())
}
