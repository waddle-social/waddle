package cilium

import (
	"strings"
	"testing"
)

func TestGatewayAPIStandardCRDsManifestIncludesRequiredResources(t *testing.T) {
	for _, required := range []string{
		"gatewayclasses.gateway.networking.k8s.io",
		"gateways.gateway.networking.k8s.io",
		"httproutes.gateway.networking.k8s.io",
		"grpcroutes.gateway.networking.k8s.io",
		"referencegrants.gateway.networking.k8s.io",
	} {
		if !strings.Contains(gatewayAPIStandardCRDsManifest, required) {
			t.Fatalf("gateway API manifest missing %q", required)
		}
	}
}
