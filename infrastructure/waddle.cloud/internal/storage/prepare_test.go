package storage

import (
	"context"
	"strings"
	"testing"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	ctrlclient "sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/client/fake"
)

func TestBuildOpenEBSMayastorPrepJob(t *testing.T) {
	job := buildOpenEBSMayastorPrepJob(PrepareOpenEBSMayastorParams{
		NodeName: "production-control-plane-01",
		Device:   "/dev/nvme1n1",
	})

	if job.Namespace != openEBSMayastorPrepNamespace {
		t.Fatalf("job namespace = %q, want %q", job.Namespace, openEBSMayastorPrepNamespace)
	}
	if job.Name != openEBSMayastorPrepJobName {
		t.Fatalf("job name = %q, want %q", job.Name, openEBSMayastorPrepJobName)
	}

	podSpec := job.Spec.Template.Spec
	if podSpec.NodeSelector["kubernetes.io/hostname"] != "production-control-plane-01" {
		t.Fatalf("node selector hostname = %q, want %q", podSpec.NodeSelector["kubernetes.io/hostname"], "production-control-plane-01")
	}
	if len(podSpec.Containers) != 1 {
		t.Fatalf("container count = %d, want 1", len(podSpec.Containers))
	}

	container := podSpec.Containers[0]
	if container.Image != openEBSLinuxUtilsImage {
		t.Fatalf("container image = %q, want %q", container.Image, openEBSLinuxUtilsImage)
	}
	if len(container.Args) != 1 {
		t.Fatalf("container args length = %d, want 1", len(container.Args))
	}
	if job.Spec.TTLSecondsAfterFinished != nil {
		t.Fatalf("expected no job ttl after finished, got %d", *job.Spec.TTLSecondsAfterFinished)
	}
	script := container.Args[0]
	if !strings.Contains(script, "/host-dev/nvme1n1") {
		t.Fatalf("prep script missing host device path:\n%s", script)
	}
	if !strings.Contains(script, "nsenter --mount=\"$HOST_MOUNT_NS\" findmnt -rn -S \"$candidate_real\"") {
		t.Fatalf("prep script missing host mount namespace check:\n%s", script)
	}
	if !strings.Contains(script, "host kernel module nvme_tcp is not loaded") {
		t.Fatalf("prep script missing nvme_tcp preflight:\n%s", script)
	}
	if !strings.Contains(script, "device $REAL_DEVICE has inactive partitions; wiping stale state") {
		t.Fatalf("prep script missing stale partition wipe behavior:\n%s", script)
	}
	if strings.Contains(script, "pvcreate -ff -y") {
		t.Fatalf("prep script should not create LVM PVs:\n%s", script)
	}
}

func TestHostDevicePath(t *testing.T) {
	if got := hostDevicePath("/dev/nvme1n1"); got != "/host-dev/nvme1n1" {
		t.Fatalf("hostDevicePath() = %q, want %q", got, "/host-dev/nvme1n1")
	}
}

func TestBuildStoragePrepFailureErrorIncludesPodLogs(t *testing.T) {
	scheme := newStorageScheme()
	kubeClient := fake.NewClientBuilder().
		WithScheme(scheme).
		WithObjects(&corev1.Pod{
			ObjectMeta: metav1.ObjectMeta{
				Name:      "waddle-storage-prep-openebs-mayastor-pod",
				Namespace: openEBSMayastorPrepNamespace,
				Labels: map[string]string{
					"job-name": openEBSMayastorPrepJobName,
				},
			},
		}).
		Build()

	previousReadPodLogsFn := readPodLogsFn
	t.Cleanup(func() {
		readPodLogsFn = previousReadPodLogsFn
	})

	readPodLogsFn = func(_ context.Context, _ kubernetes.Interface, namespace, podName string) (string, error) {
		if namespace != openEBSMayastorPrepNamespace {
			t.Fatalf("namespace = %q, want %q", namespace, openEBSMayastorPrepNamespace)
		}
		if podName != "waddle-storage-prep-openebs-mayastor-pod" {
			t.Fatalf("pod name = %q, want %q", podName, "waddle-storage-prep-openebs-mayastor-pod")
		}
		return "device /dev/nvme1n1 or child /dev/nvme1n1p1 is mounted", nil
	}

	err := buildStoragePrepFailureError(
		context.Background(),
		kubeClient,
		nil,
		ctrlclient.ObjectKey{Name: openEBSMayastorPrepJobName, Namespace: openEBSMayastorPrepNamespace},
		"storage prep job failed: Job has reached the specified backoff limit",
	)
	if err == nil {
		t.Fatal("expected failure error, got nil")
	}

	if !strings.Contains(err.Error(), "storage prep job failed: Job has reached the specified backoff limit") {
		t.Fatalf("expected failure message in error, got %v", err)
	}
	if !strings.Contains(err.Error(), "pod waddle-storage-prep-openebs-mayastor-pod logs:") {
		t.Fatalf("expected pod logs prefix in error, got %v", err)
	}
	if !strings.Contains(err.Error(), "device /dev/nvme1n1 or child /dev/nvme1n1p1 is mounted") {
		t.Fatalf("expected pod log content in error, got %v", err)
	}
}
