package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/rawkode-academy/rawkode-cloud3/internal/config"
	"github.com/rawkode-academy/rawkode-cloud3/internal/operation"
)

func TestIngressPublicIPv4ForOperationUsesDiscoveredPublicIP(t *testing.T) {
	op := operation.New("op-test", operation.TypeCreateCluster, "production", nil)
	op.SetContext("publicIP", "51.159.1.2")

	got, err := ingressPublicIPv4ForOperation(&config.Config{}, op)
	if err != nil {
		t.Fatalf("ingressPublicIPv4ForOperation returned error: %v", err)
	}
	if got != "51.159.1.2" {
		t.Fatalf("ingressPublicIPv4ForOperation() = %q, want %q", got, "51.159.1.2")
	}
}

func TestIngressPublicIPv4ForOperationUsesOverride(t *testing.T) {
	op := operation.New("op-test", operation.TypeCreateCluster, "production", nil)
	op.SetContext("publicIP", "51.159.1.2")

	got, err := ingressPublicIPv4ForOperation(&config.Config{
		Ingress: config.IngressConfig{
			PublicIPv4: "203.0.113.10",
		},
	}, op)
	if err != nil {
		t.Fatalf("ingressPublicIPv4ForOperation returned error: %v", err)
	}
	if got != "203.0.113.10" {
		t.Fatalf("ingressPublicIPv4ForOperation() = %q, want %q", got, "203.0.113.10")
	}
}

func TestIngressPublicIPv4ForOperationRequiresIPv4(t *testing.T) {
	op := operation.New("op-test", operation.TypeCreateCluster, "production", nil)
	op.SetContext("publicIP", "not-an-ip")

	if _, err := ingressPublicIPv4ForOperation(&config.Config{}, op); err == nil {
		t.Fatal("expected ingressPublicIPv4ForOperation validation error")
	}
}

func TestIngressManifestsNoLongerContainLegacyVIPOrRouteTargetAnnotations(t *testing.T) {
	files := []struct {
		path            string
		forbiddenValues []string
	}{
		{
			path: filepath.Join("..", "..", "platform", "infrastructure", "cilium-gateway", "gateway.yaml"),
			forbiddenValues: []string{
				"10.10.0.30",
				externalDNSTargetAnnotation + ":",
				"addresses:",
			},
		},
		{
			path:            filepath.Join("..", "..", "platform", "apps", "waddle-server", "httproute.yaml"),
			forbiddenValues: []string{externalDNSTargetAnnotation + ":"},
		},
		{
			path:            filepath.Join("..", "..", "platform", "apps", "demo", "httproute.yaml"),
			forbiddenValues: []string{externalDNSTargetAnnotation + ":"},
		},
		{
			path:            filepath.Join("..", "..", "platform", "apps", "spicedb", "httproute.yaml"),
			forbiddenValues: []string{externalDNSTargetAnnotation + ":"},
		},
	}

	for _, file := range files {
		content, err := os.ReadFile(file.path)
		if err != nil {
			t.Fatalf("read %s: %v", file.path, err)
		}

		text := string(content)
		for _, forbidden := range file.forbiddenValues {
			if strings.Contains(text, forbidden) {
				t.Fatalf("%s still contains legacy ingress value %q", file.path, forbidden)
			}
		}
	}
}
