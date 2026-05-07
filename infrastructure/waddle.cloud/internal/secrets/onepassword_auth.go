package secrets

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strings"
)

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
