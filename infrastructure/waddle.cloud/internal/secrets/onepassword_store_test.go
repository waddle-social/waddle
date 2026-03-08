package secrets

import (
	"context"
	"errors"
	"os/exec"
	"strings"
	"testing"
)

type scriptedResponse struct {
	assert func(t *testing.T, name string, args []string, stdin []byte)
	stdout string
	stderr string
	err    error
}

type scriptedRunner struct {
	t         *testing.T
	responses []scriptedResponse
}

func (r *scriptedRunner) Run(_ context.Context, name string, args []string, stdin []byte) ([]byte, []byte, error) {
	r.t.Helper()
	if len(r.responses) == 0 {
		r.t.Fatalf("unexpected command: %s %v", name, args)
	}

	resp := r.responses[0]
	r.responses = r.responses[1:]
	if resp.assert != nil {
		resp.assert(r.t, name, args, stdin)
	}

	return []byte(resp.stdout), []byte(resp.stderr), resp.err
}

func TestNewOnePasswordStoreUsesLocalAuthContext(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{
				assert: func(t *testing.T, name string, args []string, _ []byte) {
					if name != "op" || len(args) < 2 || args[0] != "whoami" {
						t.Fatalf("unexpected whoami call: %s %v", name, args)
					}
				},
				stdout: "{}",
			},
		},
	}

	_, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err != nil {
		t.Fatalf("newOnePasswordStoreWithRunner returned error: %v", err)
	}
}

func TestNewOnePasswordStoreReportsMissingOPBinary(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{
				err: exec.ErrNotFound,
			},
		},
	}

	_, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err == nil {
		t.Fatal("expected missing op binary error")
	}
	if !strings.Contains(err.Error(), "\"op\" was not found in PATH") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestNewOnePasswordStoreReportsLocalSigninRequirement(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{
				stderr: "[ERROR] You are not currently signed in. Use `op signin` to sign in.",
				err:    errors.New("exit status 1"),
			},
		},
	}

	_, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err == nil {
		t.Fatal("expected local sign-in error")
	}
	if !strings.Contains(err.Error(), "sign in locally with `op`") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestNewOnePasswordStorePassesAccountToWhoami(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{
				assert: func(t *testing.T, name string, args []string, _ []byte) {
					if name != "op" {
						t.Fatalf("unexpected command: %s", name)
					}
					expected := []string{"whoami", "--format", "json", "--account", "waddle-social.1password.eu"}
					if len(args) != len(expected) {
						t.Fatalf("unexpected args length: got %v, want %v", args, expected)
					}
					for i := range expected {
						if args[i] != expected[i] {
							t.Fatalf("args[%d] = %q, want %q (full args=%v)", i, args[i], expected[i], args)
						}
					}
				},
				stdout: "{}",
			},
		},
	}

	_, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{
		Vault:   "Employee",
		Account: "waddle-social.1password.eu",
	}, runner)
	if err != nil {
		t.Fatalf("newOnePasswordStoreWithRunner returned error: %v", err)
	}
}

func TestEnsurePathCreatesMissingItem(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{
				assert: func(t *testing.T, name string, args []string, _ []byte) {
					if name != "op" || len(args) < 2 || args[0] != "whoami" {
						t.Fatalf("unexpected whoami call: %s %v", name, args)
					}
				},
				stdout: "{}",
			},
			{
				assert: func(t *testing.T, name string, args []string, _ []byte) {
					if name != "op" || args[0] != "item" || args[1] != "get" {
						t.Fatalf("unexpected get call: %s %v", name, args)
					}
				},
				stderr: "[ERROR] \"waddle-cloud:path:/projects/waddle-cloud\" isn't an item",
				err:    errors.New("exit status 1"),
			},
			{
				assert: func(t *testing.T, name string, args []string, _ []byte) {
					if name != "op" || args[0] != "item" || args[1] != "create" {
						t.Fatalf("unexpected create call: %s %v", name, args)
					}
				},
				stdout: `{"id":"item-1","title":"waddle-cloud:path:/projects/waddle-cloud","fields":[]}`,
			},
		},
	}

	storeAny, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err != nil {
		t.Fatalf("newOnePasswordStoreWithRunner returned error: %v", err)
	}

	store := storeAny.(*onePasswordStore)
	if err := store.EnsurePath(context.Background(), "/projects/waddle-cloud"); err != nil {
		t.Fatalf("EnsurePath returned error: %v", err)
	}
	if len(runner.responses) != 0 {
		t.Fatalf("unused scripted responses: %d", len(runner.responses))
	}
}

func TestSetSecretUpsertsMultilineValue(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{stdout: "{}"},
			{stdout: `{"id":"item-123","title":"waddle-cloud:path:/projects/waddle-cloud","fields":[]}`},
			{
				assert: func(t *testing.T, name string, args []string, stdin []byte) {
					if name != "op" || args[0] != "item" || args[1] != "edit" {
						t.Fatalf("unexpected edit call: %s %v", name, args)
					}
					body := string(stdin)
					if !strings.Contains(body, `"label":"TALOSCONFIG_YAML"`) {
						t.Fatalf("expected TALOSCONFIG_YAML label in payload: %s", body)
					}
					if !strings.Contains(body, `line1\nline2`) {
						t.Fatalf("expected multiline value in payload: %s", body)
					}
					if !strings.Contains(body, `"section":{"id":"waddle-cloud","label":"waddle-cloud"}`) {
						t.Fatalf("expected managed section in payload: %s", body)
					}
				},
			},
		},
	}

	storeAny, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err != nil {
		t.Fatalf("newOnePasswordStoreWithRunner returned error: %v", err)
	}

	store := storeAny.(*onePasswordStore)
	err = store.SetSecret(context.Background(), "/projects/waddle-cloud", "TALOSCONFIG_YAML", "line1\nline2")
	if err != nil {
		t.Fatalf("SetSecret returned error: %v", err)
	}
	if len(runner.responses) != 0 {
		t.Fatalf("unused scripted responses: %d", len(runner.responses))
	}
}

func TestGetSecretsReturnsManagedSectionOnly(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{stdout: "{}"},
			{stdout: `{
				"id":"item-123",
				"fields":[
					{"label":"TALOSCONFIG_YAML","value":"abc","section":{"id":"waddle-cloud","label":"waddle-cloud"}},
					{"label":"username","value":"admin","section":{"id":"credentials","label":"credentials"}}
				]
			}`},
		},
	}

	storeAny, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err != nil {
		t.Fatalf("newOnePasswordStoreWithRunner returned error: %v", err)
	}

	store := storeAny.(*onePasswordStore)
	secrets, err := store.GetSecrets(context.Background(), "/projects/waddle-cloud")
	if err != nil {
		t.Fatalf("GetSecrets returned error: %v", err)
	}
	if len(secrets) != 1 {
		t.Fatalf("expected 1 managed secret, got %d (%v)", len(secrets), secrets)
	}
	if got := secrets["TALOSCONFIG_YAML"]; got != "abc" {
		t.Fatalf("managed secret value = %q, want %q", got, "abc")
	}
}

func TestGetSecretReturnsErrSecretNotFoundWhenKeyMissing(t *testing.T) {
	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{stdout: "{}"},
			{stdout: `{"id":"item-123","fields":[]}`},
		},
	}

	storeAny, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err != nil {
		t.Fatalf("newOnePasswordStoreWithRunner returned error: %v", err)
	}

	store := storeAny.(*onePasswordStore)
	_, err = store.GetSecret(context.Background(), "/projects/waddle-cloud", "TALOSCONFIG_YAML")
	if err == nil {
		t.Fatal("expected missing key error")
	}
	if !errors.Is(err, ErrSecretNotFound) {
		t.Fatalf("expected ErrSecretNotFound, got %v", err)
	}
}
