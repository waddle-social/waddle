package scaleway

import (
	"encoding/base64"
	"encoding/json"
	"testing"
)

func TestInstallUserDataFileBase64EncodesContent(t *testing.T) {
	t.Parallel()

	cloudInit := []byte("#cloud-config\nruncmd:\n  - echo hello\n")

	payload, err := json.Marshal(installUserDataFile(cloudInit))
	if err != nil {
		t.Fatalf("marshal install user-data file: %v", err)
	}

	var encoded struct {
		Name        string `json:"name"`
		ContentType string `json:"content_type"`
		Content     string `json:"content"`
	}
	if err := json.Unmarshal(payload, &encoded); err != nil {
		t.Fatalf("unmarshal encoded install user-data file: %v", err)
	}

	if encoded.Name != "user-data" {
		t.Fatalf("name = %q, want %q", encoded.Name, "user-data")
	}
	if encoded.ContentType != "text/plain" {
		t.Fatalf("content type = %q, want %q", encoded.ContentType, "text/plain")
	}

	want := base64.StdEncoding.EncodeToString(cloudInit)
	if encoded.Content != want {
		t.Fatalf("content = %q, want %q", encoded.Content, want)
	}

	decoded, err := base64.StdEncoding.DecodeString(encoded.Content)
	if err != nil {
		t.Fatalf("decode content: %v", err)
	}
	if string(decoded) != string(cloudInit) {
		t.Fatalf("decoded content = %q, want %q", string(decoded), string(cloudInit))
	}
}
