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

func TestGitOpsRootDoesNotSplitWaddleRuntimeStack(t *testing.T) {
	root := readYAML[resourceKustomization](t, gitOpsPath("kustomization.yaml"))
	for _, forbidden := range []string{
		"kustomization-infra-cloudnative-pg-cluster.yaml",
		"kustomization-infra-spicedb.yaml",
	} {
		if slices.Contains(root.Resources, forbidden) {
			t.Fatalf("root gitops kustomization still splits runtime stack via %q: %#v", forbidden, root.Resources)
		}
	}
}

func TestWaddleServerGitOpsUsesRuntimeSpiceDBSecretsOnly(t *testing.T) {
	waddleServer := readYAML[resourceKustomization](t, gitOpsPath("waddle-server", "kustomization.yaml"))
	for _, expected := range []string{
		"postgresql-cluster.yaml",
		"spicedb-postgres-app-user-external-secret.yaml",
		"spicedb-config-external-secret.yaml",
		"spicedb-postgres-cluster.yaml",
		"spicedb-cluster.yaml",
		"spicedb-external-secret.yaml",
	} {
		if !slices.Contains(waddleServer.Resources, expected) {
			t.Fatalf("waddle-server kustomization missing %q: %#v", expected, waddleServer.Resources)
		}
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
	if helmRelease.Spec.Values.SpiceDB.Endpoint != "http://spicedb:50051" {
		t.Fatalf(
			"spicedb endpoint = %q, want %q",
			helmRelease.Spec.Values.SpiceDB.Endpoint,
			"http://spicedb:50051",
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
