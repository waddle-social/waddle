package scaleway

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

var githubKeysFetcher = fetchGitHubUserPublicKeys

func fetchGitHubUserPublicKeys(ctx context.Context, username string) ([]string, error) {
	url := fmt.Sprintf("https://github.com/%s.keys", username)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, fmt.Errorf("build github keys request: %w", err)
	}

	client := &http.Client{Timeout: 10 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("fetch github keys: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetch github keys: unexpected status %s", resp.Status)
	}

	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, fmt.Errorf("read github keys response: %w", err)
	}

	lines := strings.Split(string(body), "\n")
	var keys []string
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if trimmed != "" && (strings.HasPrefix(trimmed, "ssh-") || strings.HasPrefix(trimmed, "ecdsa-") || strings.HasPrefix(trimmed, "sk-")) {
			keys = append(keys, trimmed)
		}
	}

	if len(keys) == 0 {
		return nil, fmt.Errorf("no SSH public keys found for %s", username)
	}

	return keys, nil
}
