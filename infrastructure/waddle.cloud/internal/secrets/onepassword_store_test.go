package secrets

import (
	"context"
	"errors"
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

func TestNewOnePasswordStoreRequiresAuthContext(t *testing.T) {
	t.Setenv("OP_SERVICE_ACCOUNT_TOKEN", "")

	_, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, &scriptedRunner{t: t})
	if err == nil {
		t.Fatal("expected OP_SERVICE_ACCOUNT_TOKEN validation error")
	}
	if !strings.Contains(err.Error(), "OP_SERVICE_ACCOUNT_TOKEN") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestEnsurePathCreatesMissingItem(t *testing.T) {
	t.Setenv("OP_SERVICE_ACCOUNT_TOKEN", "token")

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
				stderr: "[ERROR] \"rawkode-cloud3:path:/projects/rawkode-cloud\" isn't an item",
				err:    errors.New("exit status 1"),
			},
			{
				assert: func(t *testing.T, name string, args []string, _ []byte) {
					if name != "op" || args[0] != "item" || args[1] != "create" {
						t.Fatalf("unexpected create call: %s %v", name, args)
					}
				},
				stdout: `{"id":"item-1","title":"rawkode-cloud3:path:/projects/rawkode-cloud","fields":[]}`,
			},
		},
	}

	storeAny, err := newOnePasswordStoreWithRunner(context.Background(), OnePasswordConfig{Vault: "Employee"}, runner)
	if err != nil {
		t.Fatalf("newOnePasswordStoreWithRunner returned error: %v", err)
	}

	store := storeAny.(*onePasswordStore)
	if err := store.EnsurePath(context.Background(), "/projects/rawkode-cloud"); err != nil {
		t.Fatalf("EnsurePath returned error: %v", err)
	}
	if len(runner.responses) != 0 {
		t.Fatalf("unused scripted responses: %d", len(runner.responses))
	}
}

func TestSetSecretUpsertsMultilineValue(t *testing.T) {
	t.Setenv("OP_SERVICE_ACCOUNT_TOKEN", "token")

	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{stdout: "{}"},
			{stdout: `{"id":"item-123","title":"rawkode-cloud3:path:/projects/rawkode-cloud","fields":[]}`},
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
					if !strings.Contains(body, `"section":{"id":"rawkode-cloud3","label":"rawkode-cloud3"}`) {
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
	err = store.SetSecret(context.Background(), "/projects/rawkode-cloud", "TALOSCONFIG_YAML", "line1\nline2")
	if err != nil {
		t.Fatalf("SetSecret returned error: %v", err)
	}
	if len(runner.responses) != 0 {
		t.Fatalf("unused scripted responses: %d", len(runner.responses))
	}
}

func TestGetSecretsReturnsManagedSectionOnly(t *testing.T) {
	t.Setenv("OP_SERVICE_ACCOUNT_TOKEN", "token")

	runner := &scriptedRunner{
		t: t,
		responses: []scriptedResponse{
			{stdout: "{}"},
			{stdout: `{
				"id":"item-123",
				"fields":[
					{"label":"TALOSCONFIG_YAML","value":"abc","section":{"id":"rawkode-cloud3","label":"rawkode-cloud3"}},
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
	secrets, err := store.GetSecrets(context.Background(), "/projects/rawkode-cloud")
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
	t.Setenv("OP_SERVICE_ACCOUNT_TOKEN", "token")

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
	_, err = store.GetSecret(context.Background(), "/projects/rawkode-cloud", "TALOSCONFIG_YAML")
	if err == nil {
		t.Fatal("expected missing key error")
	}
	if !errors.Is(err, ErrSecretNotFound) {
		t.Fatalf("expected ErrSecretNotFound, got %v", err)
	}
}
