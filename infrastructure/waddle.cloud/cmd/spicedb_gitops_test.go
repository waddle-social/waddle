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
			Secret          struct {
				RuntimeSecretName string `yaml:"runtimeSecretName"`
			} `yaml:"secret"`
			SpiceDB struct {
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

func externalSecretPaths(t *testing.T) []string {
	t.Helper()

	paths, err := filepath.Glob(gitOpsPath("waddle-server", "*external-secret.yaml"))
	if err != nil {
		t.Fatalf("glob external secrets: %v", err)
	}
	if len(paths) == 0 {
		t.Fatal("waddle-server gitops has no ExternalSecret manifests")
	}

	return paths
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
		"runtime-external-secret.yaml",
	} {
		if !slices.Contains(waddleServer.Resources, expected) {
			t.Fatalf("waddle-server kustomization missing %q: %#v", expected, waddleServer.Resources)
		}
	}
	for _, removed := range []string{
		"external-secret.yaml",
		"spicedb-external-secret.yaml",
	} {
		if slices.Contains(waddleServer.Resources, removed) {
			t.Fatalf("waddle-server kustomization still references removed runtime secret %q", removed)
		}
	}

	externalSecretPath := gitOpsPath("waddle-server", "runtime-external-secret.yaml")
	externalSecret := readYAML[externalSecretManifest](t, externalSecretPath)
	if got, want := len(externalSecret.Spec.Data), 5; got != want {
		t.Fatalf("runtime ExternalSecret mapping count = %d, want %d: %#v", got, want, externalSecret.Spec.Data)
	}
	remoteRefs := map[string]struct {
		key      string
		property string
	}{}
	for _, entry := range externalSecret.Spec.Data {
		remoteRefs[entry.SecretKey] = struct {
			key      string
			property string
		}{
			key:      entry.RemoteRef.Key,
			property: entry.RemoteRef.Property,
		}
	}
	for secretKey, property := range map[string]string{
		"WADDLE_SESSION_KEY":           "session-key",
		"WADDLE_OCCUPANT_ID_SECRET":    "occupant-id-secret",
		"WADDLE_S3_ACCESS_KEY_ID":      "r2-access-key-id",
		"WADDLE_S3_SECRET_ACCESS_KEY":  "r2-secret-access-key",
		"WADDLE_SPICEDB_PRESHARED_KEY": "spicedb-preshared-key",
	} {
		ref, ok := remoteRefs[secretKey]
		if !ok {
			t.Fatalf("runtime ExternalSecret missing %s mapping: %#v", secretKey, remoteRefs)
		}
		if ref.key != "server-runtime-production" || ref.property != property {
			t.Fatalf(
				"%s remote ref = %q/%q, want %q/%q",
				secretKey,
				ref.key,
				ref.property,
				"server-runtime-production",
				property,
			)
		}
	}

	openRouterSecret := readYAML[externalSecretManifest](t, gitOpsPath("waddle-server", "openrouter-external-secret.yaml"))
	if got, want := len(openRouterSecret.Spec.Data), 1; got != want {
		t.Fatalf("openrouter ExternalSecret mapping count = %d, want %d: %#v", got, want, openRouterSecret.Spec.Data)
	}
	if entry := openRouterSecret.Spec.Data[0]; entry.SecretKey != "apiKey" ||
		entry.RemoteRef.Key != "server-runtime-production" ||
		entry.RemoteRef.Property != "openrouter-api-key" {
		t.Fatalf("openrouter remote ref = %#v, want apiKey from server-runtime-production/openrouter-api-key", entry)
	}

	spicedbConfigSecret := readYAML[externalSecretManifest](t, gitOpsPath("waddle-server", "spicedb-config-external-secret.yaml"))
	foundSpicedbPresharedKey := false
	for _, entry := range spicedbConfigSecret.Spec.Data {
		if entry.SecretKey != "preshared_key" {
			continue
		}
		foundSpicedbPresharedKey = true
		if entry.RemoteRef.Key != "server-runtime-production" || entry.RemoteRef.Property != "spicedb-preshared-key" {
			t.Fatalf("spicedb preshared remote ref = %#v, want server-runtime-production/spicedb-preshared-key", entry)
		}
	}
	if !foundSpicedbPresharedKey {
		t.Fatalf("spicedb-config ExternalSecret missing preshared_key mapping: %#v", spicedbConfigSecret.Spec.Data)
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
	if helmRelease.Spec.Values.Secret.RuntimeSecretName != "waddle-runtime-secrets" {
		t.Fatalf(
			"secret.runtimeSecretName = %q, want %q",
			helmRelease.Spec.Values.Secret.RuntimeSecretName,
			"waddle-runtime-secrets",
		)
	}
	if got, want := helmRelease.Spec.Values.ExtraSecretRefs, []string{"waddle-runtime-secrets"}; !slices.Equal(got, want) {
		t.Fatalf(
			"extraSecretRefs = %#v, want %#v",
			got,
			want,
		)
	}
	for _, removed := range []string{"waddle-spicedb", "waddle-r2-credentials"} {
		if slices.Contains(helmRelease.Spec.Values.ExtraSecretRefs, removed) {
			t.Fatalf("extraSecretRefs still references removed runtime Secret %q", removed)
		}
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

func TestWaddleServerExternalSecretsDoNotUseDefaultPasswordField(t *testing.T) {
	for _, path := range externalSecretPaths(t) {
		manifest := readYAML[externalSecretManifest](t, path)
		for _, entry := range manifest.Spec.Data {
			if entry.RemoteRef.Property == "password" {
				t.Fatalf("%s maps %s from the 1Password default password field", path, entry.SecretKey)
			}
		}
	}
}
