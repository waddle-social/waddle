package cmd

import (
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

const egressChartPath = "../charts/livekit-egress"

// egressBaselineValues is a production-equivalent value set (secrets stubbed
// with CI placeholders, exactly as env.cue's helm lint passes them).
var egressBaselineValues = []string{
	"egress.wsUrl=ws://livekit-sfu.livekit.svc.cluster.local:7880",
	"egress.redisAddress=livekit-redis.livekit.svc.cluster.local:6379",
	"egress.s3.region=auto",
	"egress.s3.endpoint=https://example.r2.cloudflarestorage.com",
	"egress.s3.bucket=waddle-social-files",
	"egress.apiKey=ci-key",
	"egress.apiSecret=ci-secret",
	"egress.s3.accessKey=ci-access",
	"egress.s3.secret=ci-secret",
}

// renderEgressTemplate runs `helm template` against the livekit-egress
// chart, optionally restricting to one template via --show-only.
func renderEgressTemplate(t *testing.T, showOnly string, overrides ...string) (string, error) {
	t.Helper()
	helm, err := exec.LookPath("helm")
	if err != nil {
		t.Skip("helm not on PATH; skipping chart-render test")
	}
	args := []string{"template", "livekit-egress", egressChartPath}
	if showOnly != "" {
		args = append(args, "--show-only", showOnly)
	}
	for _, v := range append(append([]string{}, egressBaselineValues...), overrides...) {
		args = append(args, "--set", v)
	}
	out, err := exec.Command(helm, args...).CombinedOutput()
	return string(out), err
}

// egressHelmRelease is the slice of the livekit-egress HelmRelease this
// suite asserts on: the connectivity + storage values the chart renders
// into the egress config.
type egressHelmRelease struct {
	Spec struct {
		Chart struct {
			Spec struct {
				Chart   string `yaml:"chart"`
				Version string `yaml:"version"`
			} `yaml:"spec"`
		} `yaml:"chart"`
		ValuesFrom []struct {
			Kind       string `yaml:"kind"`
			Name       string `yaml:"name"`
			ValuesKey  string `yaml:"valuesKey"`
			TargetPath string `yaml:"targetPath"`
		} `yaml:"valuesFrom"`
		Values struct {
			Egress struct {
				WsURL        string `yaml:"wsUrl"`
				RedisAddress string `yaml:"redisAddress"`
				S3           struct {
					Region   string `yaml:"region"`
					Endpoint string `yaml:"endpoint"`
					Bucket   string `yaml:"bucket"`
				} `yaml:"s3"`
			} `yaml:"egress"`
		} `yaml:"values"`
	} `yaml:"spec"`
}

// chartsPath resolves a path under the sibling charts/ tree, mirroring
// gitOpsPath's convention for the gitops/ tree.
func chartsPath(parts ...string) string {
	allParts := append([]string{"..", "charts"}, parts...)
	return filepath.Join(allParts...)
}

type chartMeta struct {
	Name    string `yaml:"name"`
	Version string `yaml:"version"`
}

var semverRe = regexp.MustCompile(`^\d+\.\d+\.\d+$`)

// AC4: the egress chart must carry a concrete semver version so a bump
// makes Flux republish the OCI chart (per the chart-publish rule).
func TestLiveKitEgressChartIsPublishable(t *testing.T) {
	chart := readYAML[chartMeta](t, chartsPath("livekit-egress", "Chart.yaml"))
	if chart.Name != "livekit-egress" {
		t.Fatalf("chart name = %q, want %q", chart.Name, "livekit-egress")
	}
	if !semverRe.MatchString(chart.Version) {
		t.Fatalf("chart version = %q, want a concrete X.Y.Z semver", chart.Version)
	}
}

// AC2 prerequisite: egress reaches the SFU over ws_url and receives jobs
// over the shared Redis bus. Both must resolve to the in-cluster services,
// never localhost or an external host.
func TestLiveKitEgressConnectsToSfuAndRedis(t *testing.T) {
	hr := readYAML[egressHelmRelease](t, gitOpsPath("livekit-egress", "helmrelease.yaml"))

	if !strings.Contains(hr.Spec.Values.Egress.WsURL, "livekit-sfu") {
		t.Fatalf("egress wsUrl = %q, want the in-cluster livekit-sfu service", hr.Spec.Values.Egress.WsURL)
	}
	if !strings.HasPrefix(hr.Spec.Values.Egress.WsURL, "ws://") {
		t.Fatalf("egress wsUrl = %q, want a ws:// SFU signal URL", hr.Spec.Values.Egress.WsURL)
	}
	if !strings.Contains(hr.Spec.Values.Egress.RedisAddress, "livekit-redis") {
		t.Fatalf("egress redisAddress = %q, want the in-cluster livekit-redis service", hr.Spec.Values.Egress.RedisAddress)
	}
}

// AC2/AC3: egress authenticates to the SFU with a dedicated egress
// key/secret pair (mirroring the webhook-key precedent — a leak can't
// forge room JWTs) and uploads to R2 with the existing server R2 creds.
// All four arrive via ExternalSecret from 1Password — never in git.
func TestLiveKitEgressExternalSecretPullsApiKeysAndR2(t *testing.T) {
	es := readYAML[externalSecretManifest](t, gitOpsPath("livekit-egress", "external-secret.yaml"))

	type ref struct{ key, property string }
	got := map[string]ref{}
	for _, entry := range es.Spec.Data {
		got[entry.SecretKey] = ref{entry.RemoteRef.Key, entry.RemoteRef.Property}
	}

	want := map[string]ref{
		// SFU auth pair — dedicated egress key, lives in the SFU's
		// 1Password item alongside api-key/webhook-key.
		"apiKey":    {"livekit-sfu", "egress-key"},
		"apiSecret": {"livekit-sfu", "egress-secret"},
		// R2 upload creds — reuse the server runtime R2 keys; no new key minted.
		"s3AccessKey": {"server-runtime-production", "r2-access-key-id"},
		"s3Secret":    {"server-runtime-production", "r2-secret-access-key"},
	}
	for secretKey, w := range want {
		g, ok := got[secretKey]
		if !ok {
			t.Fatalf("egress ExternalSecret missing %s mapping: %#v", secretKey, got)
		}
		if g.key != w.key || g.property != w.property {
			t.Fatalf("%s remote ref = %q/%q, want %q/%q", secretKey, g.key, g.property, w.key, w.property)
		}
	}
}

// serverS3HelmRelease reads waddle-server's R2 target so the egress upload
// destination can be pinned to it.
type serverS3HelmRelease struct {
	Spec struct {
		Values struct {
			Config struct {
				S3Endpoint string `yaml:"s3Endpoint"`
				S3Bucket   string `yaml:"s3Bucket"`
			} `yaml:"config"`
		} `yaml:"values"`
	} `yaml:"spec"`
}

// AC3: egress must write to the SAME R2 bucket/endpoint waddle-server
// already uses. Kept in lockstep (cf. verify_turn_udp.go) so a typo can't
// point recordings at a non-existent bucket and silently fail the upload.
func TestLiveKitEgressUploadsToExistingR2Bucket(t *testing.T) {
	egress := readYAML[egressHelmRelease](t, gitOpsPath("livekit-egress", "helmrelease.yaml"))
	server := readYAML[serverS3HelmRelease](t, gitOpsPath("waddle-server", "helmrelease.yaml"))

	cfg := server.Spec.Values.Config
	if cfg.S3Bucket == "" || cfg.S3Endpoint == "" {
		t.Fatalf("waddle-server S3 target unset: bucket=%q endpoint=%q", cfg.S3Bucket, cfg.S3Endpoint)
	}
	if egress.Spec.Values.Egress.S3.Bucket != cfg.S3Bucket {
		t.Fatalf("egress s3 bucket = %q, want waddle-server's %q", egress.Spec.Values.Egress.S3.Bucket, cfg.S3Bucket)
	}
	if egress.Spec.Values.Egress.S3.Endpoint != cfg.S3Endpoint {
		t.Fatalf("egress s3 endpoint = %q, want waddle-server's %q", egress.Spec.Values.Egress.S3.Endpoint, cfg.S3Endpoint)
	}
}

// sfuExternalSecret reads the SFU api-keys ExternalSecret: both the
// 1Password remoteRefs and the rendered keys.yaml template.
type sfuExternalSecret struct {
	Spec struct {
		Target struct {
			Template struct {
				Data map[string]string `yaml:"data"`
			} `yaml:"template"`
		} `yaml:"target"`
		Data []struct {
			SecretKey string `yaml:"secretKey"`
			RemoteRef struct {
				Key      string `yaml:"key"`
				Property string `yaml:"property"`
			} `yaml:"remoteRef"`
		} `yaml:"data"`
	} `yaml:"spec"`
}

// AC2: egress authenticates to the SFU with the dedicated egress-key pair,
// but the SFU only accepts API keys present in its keys.yaml. Pull the
// egress pair from the SAME 1Password item the egress ExternalSecret uses
// AND render it into the SFU keys.yaml, or egress joins are rejected and
// nothing records. This pins both halves so they can't drift.
func TestSfuKeysAcceptEgressCredential(t *testing.T) {
	sfu := readYAML[sfuExternalSecret](t, gitOpsPath("livekit-sfu", "external-secret.yaml"))

	refs := map[string]struct{ key, property string }{}
	for _, d := range sfu.Spec.Data {
		refs[d.SecretKey] = struct{ key, property string }{d.RemoteRef.Key, d.RemoteRef.Property}
	}
	for secretKey, property := range map[string]string{
		"egressKey":    "egress-key",
		"egressSecret": "egress-secret",
	} {
		r, ok := refs[secretKey]
		if !ok {
			t.Fatalf("SFU ExternalSecret missing %s remoteRef: %#v", secretKey, refs)
		}
		if r.key != "livekit-sfu" || r.property != property {
			t.Fatalf("SFU %s remoteRef = %q/%q, want livekit-sfu/%q", secretKey, r.key, r.property, property)
		}
	}

	// The egress ExternalSecret must read the SAME 1Password properties so
	// the key NAME/secret the egress signs with matches what the SFU accepts.
	egress := readYAML[externalSecretManifest](t, gitOpsPath("livekit-egress", "external-secret.yaml"))
	egressRefs := map[string]string{}
	for _, d := range egress.Spec.Data {
		egressRefs[d.SecretKey] = d.RemoteRef.Key + "/" + d.RemoteRef.Property
	}
	if egressRefs["apiKey"] != "livekit-sfu/egress-key" || egressRefs["apiSecret"] != "livekit-sfu/egress-secret" {
		t.Fatalf("egress auth refs = %q/%q, want livekit-sfu/egress-key and -secret", egressRefs["apiKey"], egressRefs["apiSecret"])
	}

	keysYAML := sfu.Spec.Target.Template.Data["keys.yaml"]
	if !strings.Contains(keysYAML, ".egressKey") || !strings.Contains(keysYAML, ".egressSecret") {
		t.Fatalf("SFU keys.yaml template does not render the egress pair; egress auth will be rejected:\n%s", keysYAML)
	}
}

// sfuRedisHelmRelease reads the SFU's LiveKit config redis address.
type sfuRedisHelmRelease struct {
	Spec struct {
		Values struct {
			Livekit struct {
				Redis struct {
					Address string `yaml:"address"`
				} `yaml:"redis"`
			} `yaml:"livekit"`
		} `yaml:"values"`
	} `yaml:"spec"`
}

// AC2: without a shared Redis the SFU cannot dispatch jobs to egress.
// The SFU must point at the same in-cluster livekit-redis the egress
// subscribes to.
func TestSfuWiredToSharedRedis(t *testing.T) {
	sfu := readYAML[sfuRedisHelmRelease](t, gitOpsPath("livekit-sfu", "helmrelease.yaml"))
	addr := sfu.Spec.Values.Livekit.Redis.Address
	if !strings.Contains(addr, "livekit-redis") {
		t.Fatalf("sfu livekit.redis.address = %q, want the in-cluster livekit-redis service", addr)
	}
}

type namespaceManifest struct {
	Kind     string `yaml:"kind"`
	Metadata struct {
		Name string `yaml:"name"`
	} `yaml:"metadata"`
}

func mustContain(t *testing.T, haystack []string, needle, what string) {
	t.Helper()
	if !slices.Contains(haystack, needle) {
		t.Fatalf("%s missing %q: %#v", what, needle, haystack)
	}
}

// Redis is the new foundational component of the LiveKit stack: it must be
// wired into Flux via the root gitops kustomization and own the `livekit`
// namespace (moved off the SFU so the SFU can dependsOn redis without a
// namespace-ownership cycle).
func TestLiveKitRedisComponentRegisteredAndOwnsNamespace(t *testing.T) {
	root := readYAML[resourceKustomization](t, gitOpsPath("kustomization.yaml"))
	mustContain(t, root.Resources, "livekit-redis-source.yaml", "root gitops kustomization")
	mustContain(t, root.Resources, "kustomization-infra-livekit-redis.yaml", "root gitops kustomization")

	redis := readYAML[resourceKustomization](t, gitOpsPath("livekit-redis", "kustomization.yaml"))
	for _, r := range []string{"namespace.yaml", "deployment.yaml", "service.yaml", "serviceaccount.yaml", "networkpolicy.yaml"} {
		mustContain(t, redis.Resources, r, "livekit-redis kustomization")
	}

	ns := readYAML[namespaceManifest](t, gitOpsPath("livekit-redis", "namespace.yaml"))
	if ns.Kind != "Namespace" || ns.Metadata.Name != "livekit" {
		t.Fatalf("livekit-redis namespace = %s/%q, want Namespace/livekit", ns.Kind, ns.Metadata.Name)
	}

	// Namespace ownership moved: the SFU must no longer create it.
	sfu := readYAML[resourceKustomization](t, gitOpsPath("livekit-sfu", "kustomization.yaml"))
	if slices.Contains(sfu.Resources, "namespace.yaml") {
		t.Fatalf("livekit-sfu kustomization still owns namespace.yaml; ownership moved to livekit-redis: %#v", sfu.Resources)
	}
}

type redisService struct {
	Spec struct {
		Selector map[string]string `yaml:"selector"`
		Ports    []struct {
			Port       int    `yaml:"port"`
			TargetPort string `yaml:"targetPort"`
		} `yaml:"ports"`
	} `yaml:"spec"`
}

type redisDeployment struct {
	Spec struct {
		Selector struct {
			MatchLabels map[string]string `yaml:"matchLabels"`
		} `yaml:"selector"`
		Template struct {
			Metadata struct {
				Labels map[string]string `yaml:"labels"`
			} `yaml:"metadata"`
			Spec struct {
				Containers []struct {
					Ports []struct {
						Name          string `yaml:"name"`
						ContainerPort int    `yaml:"containerPort"`
					} `yaml:"ports"`
				} `yaml:"containers"`
			} `yaml:"spec"`
		} `yaml:"template"`
	} `yaml:"spec"`
}

// The Redis bus is the new critical path for AC2 (no bus → egress never
// receives jobs). Assert the Service actually routes to the Deployment's
// pod and port, and that the instance label both halves (and the SFU/egress
// NetworkPolicies) key on is consistent — a named-port or label rename on
// one side would otherwise pass every other test while silently breaking
// dispatch.
func TestLiveKitRedisServiceTargetsDeployment(t *testing.T) {
	svc := readYAML[redisService](t, gitOpsPath("livekit-redis", "service.yaml"))
	dep := readYAML[redisDeployment](t, gitOpsPath("livekit-redis", "deployment.yaml"))

	if len(svc.Spec.Ports) == 0 || svc.Spec.Ports[0].Port != 6379 {
		t.Fatalf("redis service must expose port 6379: %#v", svc.Spec.Ports)
	}
	targetPort := svc.Spec.Ports[0].TargetPort

	if len(dep.Spec.Template.Spec.Containers) == 0 {
		t.Fatal("redis deployment has no containers")
	}
	var named bool
	for _, p := range dep.Spec.Template.Spec.Containers[0].Ports {
		if p.Name == targetPort && p.ContainerPort == 6379 {
			named = true
		}
	}
	if !named {
		t.Fatalf("redis service targetPort %q does not resolve to a container port named %q on 6379: %#v",
			targetPort, targetPort, dep.Spec.Template.Spec.Containers[0].Ports)
	}

	const wantInstance = "livekit-redis"
	if svc.Spec.Selector["app.kubernetes.io/instance"] != wantInstance {
		t.Fatalf("redis service selector instance = %q, want %q", svc.Spec.Selector["app.kubernetes.io/instance"], wantInstance)
	}
	if dep.Spec.Selector.MatchLabels["app.kubernetes.io/instance"] != wantInstance ||
		dep.Spec.Template.Metadata.Labels["app.kubernetes.io/instance"] != wantInstance {
		t.Fatalf("redis deployment instance label must be %q on both selector and pod template (NetworkPolicies key on it)", wantInstance)
	}
}

// fluxKustomization reads a Flux Kustomization's source binding and
// dependency ordering.
type fluxKustomization struct {
	Spec struct {
		DependsOn []struct {
			Name string `yaml:"name"`
		} `yaml:"dependsOn"`
		SourceRef struct {
			Kind string `yaml:"kind"`
			Name string `yaml:"name"`
		} `yaml:"sourceRef"`
	} `yaml:"spec"`
}

func (k fluxKustomization) dependsOn(name string) bool {
	for _, d := range k.Spec.DependsOn {
		if d.Name == name {
			return true
		}
	}
	return false
}

// Egress must be a wired-up Flux component that orders AFTER the SFU (it
// connects to a running SFU) and after the secret stack (its api-keys +
// R2 creds come from 1Password).
func TestLiveKitEgressComponentRegistered(t *testing.T) {
	root := readYAML[resourceKustomization](t, gitOpsPath("kustomization.yaml"))
	mustContain(t, root.Resources, "livekit-egress-source.yaml", "root gitops kustomization")
	mustContain(t, root.Resources, "kustomization-infra-livekit-egress.yaml", "root gitops kustomization")

	egress := readYAML[resourceKustomization](t, gitOpsPath("livekit-egress", "kustomization.yaml"))
	for _, r := range []string{"external-secret.yaml", "helmrepository.yaml", "helmrelease.yaml", "networkpolicy.yaml"} {
		mustContain(t, egress.Resources, r, "livekit-egress kustomization")
	}

	infra := readYAML[fluxKustomization](t, gitOpsPath("kustomization-infra-livekit-egress.yaml"))
	if infra.Spec.SourceRef.Name != "livekit-egress" {
		t.Fatalf("infra-livekit-egress sourceRef = %q, want livekit-egress", infra.Spec.SourceRef.Name)
	}
	for _, dep := range []string{"infra-livekit-sfu", "infra-onepassword-connect", "infra-external-secrets"} {
		if !infra.dependsOn(dep) {
			t.Fatalf("infra-livekit-egress must dependsOn %q: %#v", dep, infra.Spec.DependsOn)
		}
	}
}

// AC1/AC2/AC3: every leg of the egress data path must be allowed out of
// the default-deny policy AND target the right peer (a port allowed to the
// wrong endpoint is still broken). Egress reaches the SFU signalling
// (7880→livekit-sfu) and its WebRTC media — the SFU advertises external-IP
// candidates (use_external_ip), so media leaves to `world` on the RTC/TURN
// ports — plus the Redis bus (6379→livekit-redis) and R2 over HTTPS
// (443→world). The SFU must additionally reach the new Redis bus, and its
// STUN egress (3478/19302→world) must NOT regress (the 2026-06-17 outage).
func TestLiveKitStackNetworkPolicyAllowsEgressPaths(t *testing.T) {
	egress := readYAML[ciliumNetworkPolicy](t, gitOpsPath("livekit-egress", "networkpolicy.yaml"))

	if !egress.egressOpensToInstance("7880", "TCP", "livekit-sfu") {
		t.Fatal("egress networkpolicy must allow 7880/TCP to the livekit-sfu endpoint (signalling)")
	}
	if !egress.egressOpensToInstance("6379", "TCP", "livekit-redis") {
		t.Fatal("egress networkpolicy must allow 6379/TCP to the livekit-redis endpoint (job bus)")
	}
	if !egress.egressOpensToEntity("443", "TCP", "world") {
		t.Fatal("egress networkpolicy must allow 443/TCP to world (R2 upload)")
	}
	// WebRTC media subscription to the SFU's external-IP candidates.
	if !egress.egressOpensToEntity("30882", "UDP", "world") || !egress.egressOpensToEntity("30881", "TCP", "world") {
		t.Fatal("egress networkpolicy must allow the SFU RTC media ports to world (30882/UDP, 30881/TCP); else egress joins the room but receives no media")
	}

	sfu := readYAML[ciliumNetworkPolicy](t, gitOpsPath("livekit-sfu", "networkpolicy.yaml"))
	if !sfu.egressOpensToInstance("6379", "TCP", "livekit-redis") {
		t.Fatal("livekit-sfu networkpolicy must allow egress 6379/TCP to the livekit-redis endpoint to dispatch egress jobs")
	}
	for _, stun := range []string{"3478", "19302"} {
		if !sfu.egressOpensToEntity(stun, "UDP", "world") {
			t.Fatalf("livekit-sfu networkpolicy dropped STUN egress %s/UDP to world — regresses external-IP detection (2026-06-17 outage)", stun)
		}
	}
}

// renderedEgressConfig is the LiveKit egress ServiceConfig shape we parse
// out of the rendered Secret. The S3 upload target MUST sit under `storage`
// (BaseConfig.StorageConfig is `yaml:"storage,omitempty"`, NOT inline); a
// top-level `s3:` is silently dropped by egress's non-strict yaml load, so
// recordings would fall back to ephemeral local disk and never reach R2.
type renderedEgressConfig struct {
	WsURL   string `yaml:"ws_url"`
	Logging struct {
		Level string `yaml:"level"`
	} `yaml:"logging"`
	// Must stay nil: egress v1.9.0 has no `log_level` key (it's `logging.level`).
	TopLevelLogLevel *string `yaml:"log_level"`
	Redis            struct {
		Address string `yaml:"address"`
	} `yaml:"redis"`
	Storage struct {
		S3 struct {
			Bucket         string `yaml:"bucket"`
			Endpoint       string `yaml:"endpoint"`
			AccessKey      string `yaml:"access_key"`
			Secret         string `yaml:"secret"`
			ForcePathStyle bool   `yaml:"force_path_style"`
		} `yaml:"s3"`
	} `yaml:"storage"`
	// Must stay nil: a top-level s3 means the storage is mis-nested.
	TopLevelS3 map[string]any `yaml:"s3"`
}

func renderEgressConfig(t *testing.T) renderedEgressConfig {
	t.Helper()
	out, err := renderEgressTemplate(t, "templates/secret.yaml")
	if err != nil {
		t.Fatalf("helm template livekit-egress secret failed: %v\n%s", err, out)
	}
	var secret struct {
		StringData struct {
			ConfigYAML string `yaml:"config.yaml"`
		} `yaml:"stringData"`
	}
	if err := yaml.Unmarshal([]byte(out), &secret); err != nil {
		t.Fatalf("parse rendered secret: %v\n%s", err, out)
	}
	var cfg renderedEgressConfig
	if err := yaml.Unmarshal([]byte(secret.StringData.ConfigYAML), &cfg); err != nil {
		t.Fatalf("parse rendered config.yaml: %v\n%s", err, secret.StringData.ConfigYAML)
	}
	return cfg
}

// The chart is a deep module over the LiveKit egress config: the operator
// sets connectivity + storage values and the chart renders a valid egress
// ServiceConfig. Assert the SFU signal URL, the Redis bus, and the R2
// target land in the CORRECT config keys (storage.s3.*, not top-level s3).
func TestLiveKitEgressChartRendersConfigFromValues(t *testing.T) {
	cfg := renderEgressConfig(t)

	if cfg.WsURL != "ws://livekit-sfu.livekit.svc.cluster.local:7880" {
		t.Fatalf("rendered ws_url = %q", cfg.WsURL)
	}
	if cfg.Redis.Address != "livekit-redis.livekit.svc.cluster.local:6379" {
		t.Fatalf("rendered redis.address = %q", cfg.Redis.Address)
	}
	if cfg.TopLevelLogLevel != nil {
		t.Fatalf("rendered config has a top-level log_level; egress uses logging.level — top-level is ignored: %q", *cfg.TopLevelLogLevel)
	}
	if cfg.Logging.Level != "info" {
		t.Fatalf("rendered logging.level = %q, want info", cfg.Logging.Level)
	}
	if cfg.TopLevelS3 != nil {
		t.Fatalf("rendered config has a TOP-LEVEL s3 key; egress requires it under storage.s3 — top-level is silently dropped: %#v", cfg.TopLevelS3)
	}
	if cfg.Storage.S3.Bucket != "waddle-social-files" {
		t.Fatalf("rendered storage.s3.bucket = %q, want waddle-social-files", cfg.Storage.S3.Bucket)
	}
	if !cfg.Storage.S3.ForcePathStyle {
		t.Fatal("rendered storage.s3.force_path_style must be true for Cloudflare R2")
	}
}

// AC2/AC3: the secret material (SFU auth + R2 creds) is NOT in git — it
// must be injected from the livekit-egress Secret into the chart values at
// render time via Flux `valuesFrom` (the same mechanism the SFU uses for
// its webhook key). Without these four bindings the deployed egress renders
// empty credentials and cannot authenticate or upload.
func TestLiveKitEgressHelmReleaseInjectsSecretsFromExternalSecret(t *testing.T) {
	hr := readYAML[egressHelmRelease](t, gitOpsPath("livekit-egress", "helmrelease.yaml"))

	type binding struct{ name, target string }
	got := map[string]binding{}
	for _, vf := range hr.Spec.ValuesFrom {
		if vf.Kind != "Secret" {
			t.Fatalf("valuesFrom %q has kind %q, want Secret", vf.ValuesKey, vf.Kind)
		}
		got[vf.ValuesKey] = binding{vf.Name, vf.TargetPath}
	}
	// valuesKey (the Secret data key) → targetPath (the chart value).
	want := map[string]string{
		"apiKey":      "egress.apiKey",
		"apiSecret":   "egress.apiSecret",
		"s3AccessKey": "egress.s3.accessKey",
		"s3Secret":    "egress.s3.secret",
	}
	for key, target := range want {
		g, ok := got[key]
		if !ok {
			t.Fatalf("egress HelmRelease missing valuesFrom for secret key %q: %#v", key, got)
		}
		if g.name != "livekit-egress" || g.target != target {
			t.Fatalf("valuesFrom %q = secret %q targetPath %q, want %q/%q", key, g.name, g.target, "livekit-egress", target)
		}
	}
}
