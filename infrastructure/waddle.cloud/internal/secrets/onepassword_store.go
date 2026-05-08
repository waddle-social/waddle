package secrets

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
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
