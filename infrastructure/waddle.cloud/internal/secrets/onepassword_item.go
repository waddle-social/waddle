package secrets

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

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
