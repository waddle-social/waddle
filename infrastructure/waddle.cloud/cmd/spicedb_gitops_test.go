package cmd

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

type resourceKustomization struct {
	Resources []string `yaml:"resources"`
}

type fluxKustomizationManifest struct {
	Spec struct {
		DependsOn []struct {
			Name string `yaml:"name"`
		} `yaml:"dependsOn"`
		Path string `yaml:"path"`
	} `yaml:"spec"`
}

type externalSecretManifest struct {
	Spec struct {
		Data []struct {
			SecretKey string `yaml:"secretKey"`
			RemoteRef struct {
				Key      string `yaml:"key"`
				Property string `yaml:"property"`
			} `yaml:"remoteRef"`
		} `yaml:"data"`
	} `yaml:"spec"`
}

type helmReleaseManifest struct {
	Spec struct {
		Values struct {
			ExtraSecretRefs []string `yaml:"extraSecretRefs"`
			SpiceDB         struct {
				Enabled  bool   `yaml:"enabled"`
				Endpoint string `yaml:"endpoint"`
				Insecure bool   `yaml:"insecure"`
			} `yaml:"spicedb"`
		} `yaml:"values"`
	} `yaml:"spec"`
}

func gitOpsPath(parts ...string) string {
	allParts := append([]string{"..", "gitops"}, parts...)
	return filepath.Join(allParts...)
}

func readYAML[T any](t *testing.T, path string) T {
	t.Helper()

	content, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}

	var manifest T
	if err := yaml.Unmarshal(content, &manifest); err != nil {
		t.Fatalf("unmarshal %s: %v", path, err)
	}

	return manifest
}

func TestGitOpsRootIncludesManagedSpiceDBStack(t *testing.T) {
	root := readYAML[resourceKustomization](t, gitOpsPath("kustomization.yaml"))
	if !slices.Contains(root.Resources, "kustomization-infra-spicedb.yaml") {
		t.Fatalf("root gitops kustomization missing infra SpiceDB stack: %#v", root.Resources)
	}

	infraSpiceDB := readYAML[fluxKustomizationManifest](t, gitOpsPath("kustomization-infra-spicedb.yaml"))
	if infraSpiceDB.Spec.Path != "./spicedb" {
		t.Fatalf("infra SpiceDB kustomization path = %q, want %q", infraSpiceDB.Spec.Path, "./spicedb")
	}

	dependsOn := make([]string, 0, len(infraSpiceDB.Spec.DependsOn))
	for _, dependency := range infraSpiceDB.Spec.DependsOn {
		dependsOn = append(dependsOn, dependency.Name)
	}
	for _, expected := range []string{
		"infra-onepassword-connect",
		"infra-cloudnative-pg",
		"infra-spicedb-operator",
	} {
		if !slices.Contains(dependsOn, expected) {
			t.Fatalf("infra SpiceDB dependencies = %#v, missing %q", dependsOn, expected)
		}
	}

	stack := readYAML[resourceKustomization](t, gitOpsPath("spicedb", "kustomization.yaml"))
	for _, expected := range []string{
		"external-secret-postgres.yaml",
		"external-secret-config.yaml",
		"postgres-cluster.yaml",
		"spicedb-cluster.yaml",
	} {
		if !slices.Contains(stack.Resources, expected) {
			t.Fatalf("spicedb stack resources = %#v, missing %q", stack.Resources, expected)
		}
	}
}

func TestWaddleServerGitOpsUsesRuntimeSpiceDBSecretsOnly(t *testing.T) {
	waddleServer := readYAML[resourceKustomization](t, gitOpsPath("waddle-server", "kustomization.yaml"))
	if !slices.Contains(waddleServer.Resources, "spicedb-external-secret.yaml") {
		t.Fatalf("waddle-server kustomization missing SpiceDB secret wiring: %#v", waddleServer.Resources)
	}

	externalSecretPath := gitOpsPath("waddle-server", "spicedb-external-secret.yaml")
	externalSecret := readYAML[externalSecretManifest](t, externalSecretPath)
	if len(externalSecret.Spec.Data) != 1 {
		t.Fatalf("expected exactly one SpiceDB secret mapping, got %#v", externalSecret.Spec.Data)
	}
	entry := externalSecret.Spec.Data[0]
	if entry.SecretKey != "WADDLE_SPICEDB_PRESHARED_KEY" {
		t.Fatalf("secret key = %q, want %q", entry.SecretKey, "WADDLE_SPICEDB_PRESHARED_KEY")
	}
	if entry.RemoteRef.Key != "spicedb" || entry.RemoteRef.Property != "preshared-key" {
		t.Fatalf(
			"remote ref = %q/%q, want %q/%q",
			entry.RemoteRef.Key,
			entry.RemoteRef.Property,
			"spicedb",
			"preshared-key",
		)
	}

	helmReleasePath := gitOpsPath("waddle-server", "helmrelease.yaml")
	helmRelease := readYAML[helmReleaseManifest](t, helmReleasePath)
	if !helmRelease.Spec.Values.SpiceDB.Enabled {
		t.Fatal("waddle-server HelmRelease must keep SpiceDB enabled")
	}
	if helmRelease.Spec.Values.SpiceDB.Endpoint != "spicedb.spicedb.svc.cluster.local:50051" {
		t.Fatalf(
			"spicedb endpoint = %q, want %q",
			helmRelease.Spec.Values.SpiceDB.Endpoint,
			"spicedb.spicedb.svc.cluster.local:50051",
		)
	}
	if !helmRelease.Spec.Values.SpiceDB.Insecure {
		t.Fatal("waddle-server HelmRelease should use in-cluster insecure SpiceDB transport")
	}
	if !slices.Contains(helmRelease.Spec.Values.ExtraSecretRefs, "waddle-spicedb") {
		t.Fatalf(
			"extraSecretRefs = %#v, missing %q",
			helmRelease.Spec.Values.ExtraSecretRefs,
			"waddle-spicedb",
		)
	}

	helmReleaseText, err := os.ReadFile(helmReleasePath)
	if err != nil {
		t.Fatalf("read %s: %v", helmReleasePath, err)
	}
	for _, forbidden := range []string{
		"bootstrapSchema",
		"schemaVersion",
		"WADDLE_SPICEDB_BOOTSTRAP_SCHEMA",
		"WADDLE_SPICEDB_SCHEMA_VERSION",
	} {
		if strings.Contains(string(helmReleaseText), forbidden) {
			t.Fatalf("%s still contains removed SpiceDB bootstrap token %q", helmReleasePath, forbidden)
		}
	}
}
