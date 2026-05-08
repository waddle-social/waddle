package talos

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"fmt"
	"io"

	"google.golang.org/protobuf/types/known/emptypb"
)

func (c *Client) Kubeconfig(ctx context.Context) ([]byte, error) {
	if c.machine == nil {
		return nil, fmt.Errorf("talos client is not initialized")
	}
	if c.insecure {
		return nil, fmt.Errorf("kubeconfig requires talosconfig")
	}

	stream, err := c.machine.Kubeconfig(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("request kubeconfig: %w", err)
	}

	var out bytes.Buffer
	for {
		chunk, err := stream.Recv()
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, fmt.Errorf("read kubeconfig stream: %w", err)
		}
		out.Write(chunk.GetBytes())
	}

	if len(bytes.TrimSpace(out.Bytes())) == 0 {
		return nil, fmt.Errorf("kubeconfig is empty")
	}

	return extractKubeconfig(out.Bytes())
}

func extractKubeconfig(data []byte) ([]byte, error) {
	gzReader, err := gzip.NewReader(bytes.NewReader(data))
	if err != nil {
		// Some Talos versions/tooling may return plain kubeconfig bytes.
		return data, nil
	}
	defer gzReader.Close() //nolint:errcheck

	tarReader := tar.NewReader(gzReader)
	var kubeconfig bytes.Buffer

	for {
		_, err := tarReader.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			// Some streams are missing trailing TAR padding blocks but still
			// contain a complete kubeconfig payload; keep what we extracted.
			if kubeconfig.Len() > 0 {
				break
			}
			return nil, fmt.Errorf("read kubeconfig tar stream: %w", err)
		}
		if _, err := io.Copy(&kubeconfig, tarReader); err != nil {
			return nil, fmt.Errorf("extract kubeconfig data: %w", err)
		}
	}

	out := kubeconfig.Bytes()
	if len(bytes.TrimSpace(out)) == 0 {
		return nil, fmt.Errorf("extracted kubeconfig is empty")
	}

	return out, nil
}

// HealthCheck verifies the node is healthy.
