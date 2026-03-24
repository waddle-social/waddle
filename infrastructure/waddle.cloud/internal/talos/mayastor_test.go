package talos

import (
	"strings"
	"testing"
)

func TestWithMayastorLabConfigAppliesPrerequisites(t *testing.T) {
	input := []byte(`machine:
  kubelet: {}
cluster:
  apiServer:
    admissionControl:
      - name: PodSecurity
        configuration:
          exemptions:
            namespaces:
              - kube-system
`)

	out, err := WithMayastorLabConfig(input)
	if err != nil {
		t.Fatalf("WithMayastorLabConfig returned error: %v", err)
	}

	content := string(out)
	for _, needle := range []string{
		"vm.nr_hugepages: \"1024\"",
		"openebs.io/engine: mayastor",
		"destination: /var/local",
		"source: /var/local",
		"- bind",
		"- rshared",
		"- rw",
		"- openebs",
	} {
		if !strings.Contains(content, needle) {
			t.Fatalf("expected output to contain %q, got:\n%s", needle, content)
		}
	}
}

func TestWithMayastorLabConfigValidatesInput(t *testing.T) {
	if _, err := WithMayastorLabConfig(nil); err == nil {
		t.Fatal("expected error for nil config")
	}
	if _, err := WithMayastorLabConfig([]byte("machine: [")); err == nil {
		t.Fatal("expected parse error for invalid yaml")
	}
}
