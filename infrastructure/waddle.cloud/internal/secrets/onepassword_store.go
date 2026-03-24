package secrets

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	pathpkg "path"
	"strings"

	"golang.org/x/term"
)

const (
	opManagedSectionLabel = "waddle-cloud"
	opManagedTag          = "waddle-cloud:managed"
	opItemTitlePrefix     = "waddle-cloud:path:"
)

type opCommandRunner interface {
	Run(ctx context.Context, name string, args []string, stdin []byte) (stdout []byte, stderr []byte, err error)
}

type opInteractiveRunner interface {
	RunInteractive(ctx context.Context, name string, args []string) error
}

type terminalChecker interface {
	IsTerminal(fd uintptr) bool
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

type execOPInteractiveRunner struct{}

func (execOPInteractiveRunner) RunInteractive(ctx context.Context, name string, args []string) error {
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

type execTerminalChecker struct{}

func (execTerminalChecker) IsTerminal(fd uintptr) bool {
	return term.IsTerminal(int(fd))
}

type onePasswordAuthStatus int

const (
	onePasswordAuthStatusOK onePasswordAuthStatus = iota
	onePasswordAuthStatusSigninRequired
	onePasswordAuthStatusOther
)

type onePasswordAuthResult struct {
	stderr []byte
	err    error
	status onePasswordAuthStatus
}

type onePasswordStore struct {
	vault             string
	account           string
	runner            opCommandRunner
	interactiveRunner opInteractiveRunner
	ttyChecker        terminalChecker
}

func NewOnePasswordStore(ctx context.Context, cfg OnePasswordConfig) (Store, error) {
	return newOnePasswordStoreWithDeps(ctx, cfg, execOPRunner{}, execOPInteractiveRunner{}, execTerminalChecker{})
}

func newOnePasswordStoreWithRunner(ctx context.Context, cfg OnePasswordConfig, runner opCommandRunner) (Store, error) {
	return newOnePasswordStoreWithDeps(ctx, cfg, runner, execOPInteractiveRunner{}, execTerminalChecker{})
}

func newOnePasswordStoreWithDeps(
	ctx context.Context,
	cfg OnePasswordConfig,
	runner opCommandRunner,
	interactiveRunner opInteractiveRunner,
	ttyChecker terminalChecker,
) (Store, error) {
	vault := strings.TrimSpace(cfg.Vault)
	if vault == "" {
		return nil, fmt.Errorf("onepassword.vault is required")
	}
	if runner == nil {
		return nil, fmt.Errorf("1password command runner is required")
	}
	if interactiveRunner == nil {
		return nil, fmt.Errorf("1password interactive command runner is required")
	}
	if ttyChecker == nil {
		return nil, fmt.Errorf("1password terminal checker is required")
	}

	store := &onePasswordStore{
		vault:             vault,
		account:           strings.TrimSpace(cfg.Account),
		runner:            runner,
		interactiveRunner: interactiveRunner,
		ttyChecker:        ttyChecker,
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

	if err := s.setManagedSecret(ctx, itemID, key, value); err != nil {
		return err
	}
	if err := s.verifyStoredSecret(ctx, path, key, value); err != nil {
		return err
	}

	return nil
}

func (s *onePasswordStore) verifyAuth(ctx context.Context) error {
	authResult := s.checkAuth(ctx)
	if authResult.err == nil {
		return nil
	}

	if authResult.status != onePasswordAuthStatusSigninRequired || !s.isInteractiveShell() {
		return formatOnePasswordAuthError(authResult.err, authResult.stderr, s.account)
	}

	if err := s.signIn(ctx); err != nil {
		return err
	}

	authResult = s.checkAuth(ctx)
	if authResult.err != nil {
		return formatOnePasswordAuthError(authResult.err, authResult.stderr, s.account)
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

func (s *onePasswordStore) setManagedSecret(ctx context.Context, itemID, key, value string) error {
	assignment := managedFieldAssignment(key, value)
	_, stderr, err := s.runOP(ctx, nil, "item", "edit", itemID, "--vault", s.vault, assignment)
	if err != nil {
		return fmt.Errorf("edit 1password item %s field %q: %w (%s)", itemID, key, err, strings.TrimSpace(string(stderr)))
	}

	return nil
}

func (s *onePasswordStore) verifyStoredSecret(ctx context.Context, path, key, want string) error {
	item, err := s.getItemByPath(ctx, path)
	if err != nil {
		return fmt.Errorf("verify persisted secret %q in path %q: %w", key, normalizeSecretPath(path), err)
	}

	got, ok := managedFieldsFromItem(item)[key]
	if !ok {
		return fmt.Errorf("verify persisted secret %q in path %q: secret field is missing after write", key, normalizeSecretPath(path))
	}
	if got != want {
		return fmt.Errorf("verify persisted secret %q in path %q: stored value did not match written value", key, normalizeSecretPath(path))
	}

	return nil
}

func (s *onePasswordStore) runOPRaw(ctx context.Context, stdin []byte, args ...string) ([]byte, []byte, error) {
	finalArgs := s.finalArgs(args...)
	stdout, stderr, err := s.runner.Run(ctx, "op", finalArgs, stdin)
	return stdout, stderr, err
}

func (s *onePasswordStore) runOP(ctx context.Context, stdin []byte, args ...string) ([]byte, []byte, error) {
	stdout, stderr, err := s.runOPRaw(ctx, stdin, args...)
	if err == nil {
		return stdout, stderr, nil
	}
	if !isOnePasswordRetryableAuthFailure(err, stderr) || !s.isInteractiveShell() {
		return stdout, stderr, err
	}
	if signInErr := s.signIn(ctx); signInErr != nil {
		return nil, nil, signInErr
	}
	return s.runOPRaw(ctx, stdin, args...)
}

func (s *onePasswordStore) checkAuth(ctx context.Context) onePasswordAuthResult {
	_, stderr, err := s.runOPRaw(ctx, nil, "whoami", "--format", "json")
	if err == nil {
		return onePasswordAuthResult{status: onePasswordAuthStatusOK}
	}

	return onePasswordAuthResult{
		stderr: stderr,
		err:    err,
		status: classifyOnePasswordAuthStatus(err, stderr),
	}
}

func (s *onePasswordStore) signIn(ctx context.Context) error {
	if err := s.runInteractiveOP(ctx, "signin"); err != nil {
		return formatOnePasswordSigninError(err, s.account)
	}
	return nil
}

func (s *onePasswordStore) runInteractiveOP(ctx context.Context, args ...string) error {
	return s.interactiveRunner.RunInteractive(ctx, "op", s.finalArgs(args...))
}

func (s *onePasswordStore) finalArgs(args ...string) []string {
	finalArgs := append([]string(nil), args...)
	if s.account != "" && !containsArg(finalArgs, "--account") {
		finalArgs = append(finalArgs, "--account", s.account)
	}
	return finalArgs
}

func (s *onePasswordStore) isInteractiveShell() bool {
	return s.ttyChecker.IsTerminal(os.Stdin.Fd()) &&
		s.ttyChecker.IsTerminal(os.Stdout.Fd()) &&
		s.ttyChecker.IsTerminal(os.Stderr.Fd())
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

func classifyOnePasswordAuthStatus(runErr error, stderr []byte) onePasswordAuthStatus {
	if runErr == nil {
		return onePasswordAuthStatusOK
	}
	if isOnePasswordRetryableAuthFailure(runErr, stderr) {
		return onePasswordAuthStatusSigninRequired
	}
	return onePasswordAuthStatusOther
}

func isOnePasswordRetryableAuthFailure(runErr error, stderr []byte) bool {
	if runErr == nil {
		return false
	}

	combined := strings.ToLower(strings.TrimSpace(runErr.Error() + " " + string(stderr)))
	return containsAny(combined,
		"not currently signed in",
		"not signed in",
		"sign in",
		"signin",
		"no accounts configured",
		"authorization timeout",
		"error initializing client",
		"session expired",
	)
}

func isOnePasswordSigninRequired(runErr error, stderr []byte) bool {
	if runErr == nil {
		return false
	}

	combined := strings.ToLower(strings.TrimSpace(runErr.Error() + " " + string(stderr)))
	return containsAny(combined,
		"not currently signed in",
		"not signed in",
		"sign in",
		"signin",
		"no accounts configured",
	)
}

func formatOnePasswordAuthError(runErr error, stderr []byte, account string) error {
	if errors.Is(runErr, exec.ErrNotFound) {
		return fmt.Errorf("1password CLI \"op\" was not found in PATH; install 1Password CLI and sign in locally")
	}

	account = strings.TrimSpace(account)
	combined := strings.ToLower(strings.TrimSpace(runErr.Error() + " " + string(stderr)))
	switch {
	case isOnePasswordSigninRequired(runErr, stderr):
		if account != "" {
			return fmt.Errorf("1password authentication failed for account %q; sign in locally with `op` and verify onepassword.account (%s)", account, strings.TrimSpace(string(stderr)))
		}
		return fmt.Errorf("1password authentication failed; sign in locally with `op` (%s)", strings.TrimSpace(string(stderr)))
	case containsAny(combined, "authorization timeout", "error initializing client", "session expired"):
		if account != "" {
			return fmt.Errorf("1password local session for account %q is not usable; unlock/sign in locally with `op` and retry (%s)", account, strings.TrimSpace(string(stderr)))
		}
		return fmt.Errorf("1password local session is not usable; unlock/sign in locally with `op` and retry (%s)", strings.TrimSpace(string(stderr)))
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

func formatOnePasswordSigninError(runErr error, account string) error {
	if errors.Is(runErr, exec.ErrNotFound) {
		return fmt.Errorf("1password CLI \"op\" was not found in PATH; install 1Password CLI and sign in locally")
	}

	account = strings.TrimSpace(account)
	if account != "" {
		return fmt.Errorf("1password interactive sign-in failed for account %q; sign in with `op signin --account %s` and retry: %w", account, account, runErr)
	}
	return fmt.Errorf("1password interactive sign-in failed; sign in with `op signin` and retry: %w", runErr)
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

	label := strings.TrimSpace(anyToString(sectionMap["label"]))
	return label == opManagedSectionLabel
}

func managedFieldAssignment(key, value string) string {
	return fmt.Sprintf("%s.%s[concealed]=%s", escapeAssignmentComponent(opManagedSectionLabel), escapeAssignmentComponent(strings.TrimSpace(key)), value)
}

func escapeAssignmentComponent(value string) string {
	replacer := strings.NewReplacer(
		`\\`, `\\\\`,
		`.`, `\.`,
		`=`, `\=`,
	)
	return replacer.Replace(value)
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
