package talos

import (
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"fmt"
	"net"
	"net/url"
	"strings"

	"gopkg.in/yaml.v3"
)

type talosconfigFile struct {
	Context  string                        `yaml:"context"`
	Contexts map[string]talosconfigContext `yaml:"contexts"`
}

type talosconfigContext struct {
	Endpoints []string `yaml:"endpoints"`
	Nodes     []string `yaml:"nodes"`
	CA        string   `yaml:"ca"`
	Crt       string   `yaml:"crt"`
	Key       string   `yaml:"key"`
}

func parseTalosconfigContext(data []byte) (*talosconfigContext, error) {
	var cfg talosconfigFile
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parse talosconfig YAML: %w", err)
	}

	if len(cfg.Contexts) == 0 {
		return nil, fmt.Errorf("talosconfig has no contexts")
	}

	contextName := strings.TrimSpace(cfg.Context)
	if contextName == "" {
		for k := range cfg.Contexts {
			contextName = k
			break
		}
	}

	ctxCfg, ok := cfg.Contexts[contextName]
	if !ok {
		return nil, fmt.Errorf("talosconfig context %q not found", contextName)
	}

	return &ctxCfg, nil
}

func tlsConfigFromContext(ctxCfg *talosconfigContext, insecure bool) (*tls.Config, error) {
	tlsConfig := &tls.Config{
		MinVersion:         tls.VersionTLS12,
		InsecureSkipVerify: insecure, //nolint:gosec
	}

	if !insecure {
		caBytes, err := decodeBase64Field("ca", ctxCfg.CA)
		if err != nil {
			return nil, err
		}

		pool := x509.NewCertPool()
		if ok := pool.AppendCertsFromPEM(caBytes); !ok {
			return nil, fmt.Errorf("invalid talosconfig CA certificate")
		}
		tlsConfig.RootCAs = pool
	}

	if strings.TrimSpace(ctxCfg.Crt) != "" || strings.TrimSpace(ctxCfg.Key) != "" {
		crtBytes, err := decodeBase64Field("crt", ctxCfg.Crt)
		if err != nil {
			return nil, err
		}
		keyBytes, err := decodeBase64Field("key", ctxCfg.Key)
		if err != nil {
			return nil, err
		}

		cert, err := tls.X509KeyPair(crtBytes, keyBytes)
		if err != nil {
			return nil, fmt.Errorf("parse talosconfig client certificate: %w", err)
		}
		tlsConfig.Certificates = []tls.Certificate{cert}
	}

	return tlsConfig, nil
}

func decodeBase64Field(fieldName, value string) ([]byte, error) {
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		return nil, fmt.Errorf("talosconfig field %q is required", fieldName)
	}

	decoded, err := base64.StdEncoding.DecodeString(trimmed)
	if err != nil {
		return nil, fmt.Errorf("decode talosconfig field %q: %w", fieldName, err)
	}

	return decoded, nil
}

func normalizeTalosEndpoint(endpoint string) (dialEndpoint, targetNode string, err error) {
	trimmed := strings.TrimSpace(endpoint)
	if trimmed == "" {
		return "", "", fmt.Errorf("endpoint is required")
	}

	hostPort := trimmed
	if strings.Contains(trimmed, "://") {
		parsed, parseErr := url.Parse(trimmed)
		if parseErr != nil {
			return "", "", fmt.Errorf("parse endpoint %q: %w", endpoint, parseErr)
		}
		if parsed.Host == "" {
			return "", "", fmt.Errorf("endpoint %q has no host", endpoint)
		}
		hostPort = parsed.Host
	}

	if host, port, splitErr := net.SplitHostPort(hostPort); splitErr == nil {
		if host == "" {
			return "", "", fmt.Errorf("endpoint %q has empty host", endpoint)
		}
		if port == "" {
			port = talosAPIDefaultPort
		}
		return net.JoinHostPort(host, port), host, nil
	}

	if strings.HasPrefix(hostPort, "[") && strings.HasSuffix(hostPort, "]") {
		host := strings.TrimSuffix(strings.TrimPrefix(hostPort, "["), "]")
		return net.JoinHostPort(host, talosAPIDefaultPort), host, nil
	}

	// IPv6 literal without brackets.
	if strings.Count(hostPort, ":") > 1 {
		return net.JoinHostPort(hostPort, talosAPIDefaultPort), hostPort, nil
	}

	return net.JoinHostPort(hostPort, talosAPIDefaultPort), hostPort, nil
}
