package secrets

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	pathpkg "path"
	"strings"
)

const (
	opManagedSectionID    = "rawkode-cloud3"
	opManagedSectionLabel = "rawkode-cloud3"
	opManagedTag          = "rawkode-cloud3:managed"
	opItemTitlePrefix     = "rawkode-cloud3:path:"
)

type opCommandRunner interface {
	Run(ctx context.Context, name string, args []string, stdin []byte) (stdout []byte, stderr []byte, err error)
}

type execOPRunner struct{}

func (execOPRunner) Run(ctx context.Context, name string, args []string, stdin []byte) ([]byte, []byte, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	if len(stdin) > 0 {
		cmd.Stdin = bytes.NewReader(stdin)
	}

	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	err := cmd.Run()
	return stdout.Bytes(), stderr.Bytes(), err
}

type onePasswordStore struct {
	vault   string
	account string
	runner  opCommandRunner
}

func NewOnePasswordStore(ctx context.Context, cfg OnePasswordConfig) (Store, error) {
	return newOnePasswordStoreWithRunner(ctx, cfg, execOPRunner{})
}

func newOnePasswordStoreWithRunner(ctx context.Context, cfg OnePasswordConfig, runner opCommandRunner) (Store, error) {
	vault := strings.TrimSpace(cfg.Vault)
	if vault == "" {
		return nil, fmt.Errorf("onepassword.vault is required")
	}
	if runner == nil {
		return nil, fmt.Errorf("1password command runner is required")
	}

	store := &onePasswordStore{
		vault:   vault,
		account: strings.TrimSpace(cfg.Account),
		runner:  runner,
	}

	if err := store.verifyAuth(ctx); err != nil {
		return nil, err
	}

	return store, nil
}

func (s *onePasswordStore) EnsurePath(ctx context.Context, path string) error {
	_, err := s.ensureItem(ctx, path)
	return err
}

func (s *onePasswordStore) GetSecret(ctx context.Context, path, key string) (string, error) {
	key = strings.TrimSpace(key)
	if key == "" {
		return "", fmt.Errorf("secret key is required")
	}

	all, err := s.GetSecrets(ctx, path)
	if err != nil {
		return "", err
	}

	value, ok := all[key]
	if !ok {
		return "", fmt.Errorf("%w: key %q in path %q", ErrSecretNotFound, key, normalizeSecretPath(path))
	}
	return value, nil
}

func (s *onePasswordStore) GetSecrets(ctx context.Context, path string) (map[string]string, error) {
	item, err := s.getItemByPath(ctx, path)
	if err != nil {
		if errors.Is(err, ErrSecretNotFound) {
			return map[string]string{}, nil
		}
		return nil, err
	}

	return managedFieldsFromItem(item), nil
}

func (s *onePasswordStore) SetSecret(ctx context.Context, path, key, value string) error {
	key = strings.TrimSpace(key)
	if key == "" {
		return fmt.Errorf("secret key is required")
	}

	item, err := s.ensureItem(ctx, path)
	if err != nil {
		return err
	}

	itemID, err := opItemID(item)
	if err != nil {
		return err
	}

	upsertManagedField(item, key, value)
	if err := s.editItem(ctx, itemID, item); err != nil {
		return err
	}

	return nil
}

func (s *onePasswordStore) verifyAuth(ctx context.Context) error {
	_, stderr, err := s.runOP(ctx, nil, "whoami", "--format", "json")
	if err != nil {
		return formatOnePasswordAuthError(err, stderr, s.account)
	}
	return nil
}

func (s *onePasswordStore) getItemByPath(ctx context.Context, path string) (map[string]any, error) {
	title := opItemTitle(normalizeSecretPath(path))
	stdout, stderr, err := s.runOP(ctx, nil, "item", "get", title, "--vault", s.vault, "--format", "json")
	if err != nil {
		if isOnePasswordItemNotFound(err, stderr) {
			return nil, ErrSecretNotFound
		}
		return nil, fmt.Errorf("get 1password item %q: %w (%s)", title, err, strings.TrimSpace(string(stderr)))
	}

	var item map[string]any
	if err := json.Unmarshal(stdout, &item); err != nil {
		return nil, fmt.Errorf("decode 1password item %q JSON: %w", title, err)
	}

	return item, nil
}

func (s *onePasswordStore) ensureItem(ctx context.Context, path string) (map[string]any, error) {
	item, err := s.getItemByPath(ctx, path)
	if err == nil {
		return item, nil
	}
	if !errors.Is(err, ErrSecretNotFound) {
		return nil, err
	}

	title := opItemTitle(normalizeSecretPath(path))
	stdout, stderr, createErr := s.runOP(ctx, nil,
		"item", "create",
		"--vault", s.vault,
		"--category", "Secure Note",
		"--title", title,
		"--tags", opManagedTag,
		"--format", "json",
	)
	if createErr != nil {
		return nil, fmt.Errorf("create 1password item %q: %w (%s)", title, createErr, strings.TrimSpace(string(stderr)))
	}

	var created map[string]any
	if err := json.Unmarshal(stdout, &created); err != nil {
		return nil, fmt.Errorf("decode created 1password item %q JSON: %w", title, err)
	}

	return created, nil
}

func (s *onePasswordStore) editItem(ctx context.Context, itemID string, item map[string]any) error {
	payload, err := json.Marshal(item)
	if err != nil {
		return fmt.Errorf("encode 1password item %s JSON: %w", itemID, err)
	}

	_, stderr, err := s.runOP(ctx, payload, "item", "edit", itemID, "--vault", s.vault)
	if err != nil {
		return fmt.Errorf("edit 1password item %s: %w (%s)", itemID, err, strings.TrimSpace(string(stderr)))
	}

	return nil
}

func (s *onePasswordStore) runOP(ctx context.Context, stdin []byte, args ...string) ([]byte, []byte, error) {
	finalArgs := append([]string(nil), args...)
	if s.account != "" && !containsArg(finalArgs, "--account") {
		finalArgs = append(finalArgs, "--account", s.account)
	}

	stdout, stderr, err := s.runner.Run(ctx, "op", finalArgs, stdin)
	return stdout, stderr, err
}

func containsArg(args []string, target string) bool {
	for _, arg := range args {
		if arg == target {
			return true
		}
	}
	return false
}

func normalizeSecretPath(path string) string {
	trimmed := strings.TrimSpace(path)
	if trimmed == "" {
		return "/"
	}

	cleaned := pathpkg.Clean("/" + strings.TrimPrefix(trimmed, "/"))
	if cleaned == "." {
		return "/"
	}
	return cleaned
}

func opItemTitle(path string) string {
	return opItemTitlePrefix + normalizeSecretPath(path)
}

func isOnePasswordItemNotFound(runErr error, stderr []byte) bool {
	if runErr == nil {
		return false
	}

	combined := strings.ToLower(strings.TrimSpace(runErr.Error() + " " + string(stderr)))
	return strings.Contains(combined, "isn't an item") ||
		strings.Contains(combined, "not found") ||
		strings.Contains(combined, "no item") ||
		strings.Contains(combined, "could not find")
}

func formatOnePasswordAuthError(runErr error, stderr []byte, account string) error {
	if errors.Is(runErr, exec.ErrNotFound) {
		return fmt.Errorf("1password CLI \"op\" was not found in PATH; install 1Password CLI and sign in locally")
	}

	account = strings.TrimSpace(account)
	combined := strings.ToLower(strings.TrimSpace(runErr.Error() + " " + string(stderr)))
	switch {
	case containsAny(combined,
		"not currently signed in",
		"not signed in",
		"sign in",
		"signin",
		"no accounts configured",
	):
		if account != "" {
			return fmt.Errorf("1password authentication failed for account %q; sign in locally with `op` and verify onepassword.account (%s)", account, strings.TrimSpace(string(stderr)))
		}
		return fmt.Errorf("1password authentication failed; sign in locally with `op` (%s)", strings.TrimSpace(string(stderr)))
	case account != "" && containsAny(combined,
		"unknown account",
		"account not found",
		"unable to resolve account",
		"invalid account",
	):
		return fmt.Errorf("1password account %q is not available in the current local `op` session (%s)", account, strings.TrimSpace(string(stderr)))
	default:
		if account != "" {
			return fmt.Errorf("verify 1password authentication context for account %q: %w (%s)", account, runErr, strings.TrimSpace(string(stderr)))
		}
		return fmt.Errorf("verify 1password authentication context: %w (%s)", runErr, strings.TrimSpace(string(stderr)))
	}
}

func containsAny(value string, patterns ...string) bool {
	for _, pattern := range patterns {
		if strings.Contains(value, pattern) {
			return true
		}
	}
	return false
}

func opItemID(item map[string]any) (string, error) {
	id := strings.TrimSpace(anyToString(item["id"]))
	if id == "" {
		return "", fmt.Errorf("1password item ID is missing")
	}
	return id, nil
}

func managedFieldsFromItem(item map[string]any) map[string]string {
	fields := itemFields(item)
	out := make(map[string]string)

	for _, rawField := range fields {
		field, ok := rawField.(map[string]any)
		if !ok {
			continue
		}
		if !isManagedSection(field["section"]) {
			continue
		}

		label := strings.TrimSpace(anyToString(field["label"]))
		if label == "" {
			continue
		}

		out[label] = anyToString(field["value"])
	}

	return out
}

func upsertManagedField(item map[string]any, key, value string) {
	fields := itemFields(item)
	updated := false

	for i, rawField := range fields {
		field, ok := rawField.(map[string]any)
		if !ok {
			continue
		}

		if !strings.EqualFold(strings.TrimSpace(anyToString(field["label"])), key) {
			continue
		}
		if !isManagedSection(field["section"]) {
			continue
		}

		field["value"] = value
		field["type"] = "CONCEALED"
		field["section"] = map[string]any{"id": opManagedSectionID, "label": opManagedSectionLabel}
		fields[i] = field
		updated = true
		break
	}

	if !updated {
		fields = append(fields, map[string]any{
			"label":   key,
			"type":    "CONCEALED",
			"value":   value,
			"section": map[string]any{"id": opManagedSectionID, "label": opManagedSectionLabel},
		})
	}

	item["fields"] = fields
}

func itemFields(item map[string]any) []any {
	rawFields, ok := item["fields"]
	if !ok {
		return []any{}
	}

	fields, ok := rawFields.([]any)
	if !ok {
		return []any{}
	}

	return fields
}

func isManagedSection(section any) bool {
	sectionMap, ok := section.(map[string]any)
	if !ok {
		return false
	}

	id := strings.TrimSpace(anyToString(sectionMap["id"]))
	label := strings.TrimSpace(anyToString(sectionMap["label"]))

	return id == opManagedSectionID || label == opManagedSectionLabel
}

func anyToString(value any) string {
	switch typed := value.(type) {
	case string:
		return typed
	case nil:
		return ""
	default:
		return fmt.Sprint(typed)
	}
}
