package storage

import (
	"context"
	"errors"
	"fmt"
	"io"
	"sort"
	"strings"
	"time"

	batchv1 "k8s.io/api/batch/v1"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/util/wait"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/tools/clientcmd"
	ctrlclient "sigs.k8s.io/controller-runtime/pkg/client"
)

const (
	openEBSMayastorPrepNamespace = "kube-system"
	openEBSMayastorPrepJobName   = "waddle-storage-prep-openebs-mayastor"
	openEBSLinuxUtilsImage       = "docker.io/openebs/linux-utils:4.4.0"
)

var errStoragePrepJobFailed = errors.New("storage prep job failed")

var readPodLogsFn = func(ctx context.Context, client kubernetes.Interface, namespace, podName string) (string, error) {
	if client == nil {
		return "", fmt.Errorf("kubernetes core client is required")
	}

	stream, err := client.CoreV1().Pods(namespace).GetLogs(podName, &corev1.PodLogOptions{}).Stream(ctx)
	if err != nil {
		return "", err
	}
	defer stream.Close()

	raw, err := io.ReadAll(stream)
	if err != nil {
		return "", err
	}

	return strings.TrimSpace(string(raw)), nil
}

// PrepareOpenEBSMayastorParams describes the host-side storage prerequisites.
type PrepareOpenEBSMayastorParams struct {
	Kubeconfig string
	NodeName   string
	Device     string
}

// PrepareOpenEBSMayastorRawDisk prepares a dedicated raw disk for a Mayastor DiskPool.
func PrepareOpenEBSMayastorRawDisk(ctx context.Context, params PrepareOpenEBSMayastorParams) error {
	if ctx == nil {
		ctx = context.Background()
	}
	if strings.TrimSpace(params.Kubeconfig) == "" {
		return fmt.Errorf("kubeconfig is required")
	}
	if strings.TrimSpace(params.NodeName) == "" {
		return fmt.Errorf("node name is required")
	}
	if strings.TrimSpace(params.Device) == "" {
		return fmt.Errorf("device is required")
	}

	restCfg, err := clientcmd.BuildConfigFromFlags("", strings.TrimSpace(params.Kubeconfig))
	if err != nil {
		return fmt.Errorf("build kubernetes config: %w", err)
	}

	kubeClient, err := ctrlclient.New(restCfg, ctrlclient.Options{Scheme: newStorageScheme()})
	if err != nil {
		return fmt.Errorf("create kubernetes client: %w", err)
	}
	coreClient, err := kubernetes.NewForConfig(restCfg)
	if err != nil {
		return fmt.Errorf("create kubernetes core client: %w", err)
	}

	job := buildOpenEBSMayastorPrepJob(params)
	key := ctrlclient.ObjectKeyFromObject(job)

	var existing batchv1.Job
	if err := kubeClient.Get(ctx, key, &existing); err == nil {
		if err := kubeClient.Delete(ctx, &existing); err != nil {
			return fmt.Errorf("delete existing storage prep job %s: %w", key.String(), err)
		}

		if err := wait.PollUntilContextTimeout(ctx, 2*time.Second, 60*time.Second, true, func(ctx context.Context) (bool, error) {
			var deleted batchv1.Job
			err := kubeClient.Get(ctx, key, &deleted)
			if apierrors.IsNotFound(err) {
				return true, nil
			}
			if err != nil {
				return false, err
			}
			return false, nil
		}); err != nil {
			return fmt.Errorf("wait for storage prep job deletion %s: %w", key.String(), err)
		}
	} else if !apierrors.IsNotFound(err) {
		return fmt.Errorf("get existing storage prep job %s: %w", key.String(), err)
	}

	if err := kubeClient.Create(ctx, job); err != nil {
		return fmt.Errorf("create storage prep job %s: %w", key.String(), err)
	}

	var failureMessage string
	if err := wait.PollUntilContextTimeout(ctx, 5*time.Second, 10*time.Minute, true, func(ctx context.Context) (bool, error) {
		var current batchv1.Job
		if err := kubeClient.Get(ctx, key, &current); err != nil {
			return false, err
		}

		for _, condition := range current.Status.Conditions {
			switch condition.Type {
			case batchv1.JobComplete:
				if condition.Status == corev1.ConditionTrue {
					return true, nil
				}
			case batchv1.JobFailed:
				if condition.Status == corev1.ConditionTrue {
					message := strings.TrimSpace(condition.Message)
					if message == "" {
						message = strings.TrimSpace(condition.Reason)
					}
					if message == "" {
						message = "unknown failure"
					}
					failureMessage = fmt.Sprintf("storage prep job failed: %s", message)
					return false, errStoragePrepJobFailed
				}
			}
		}

		if current.Status.Succeeded > 0 {
			return true, nil
		}
		if current.Status.Failed > 0 && current.Spec.BackoffLimit != nil && current.Status.Failed > *current.Spec.BackoffLimit {
			failureMessage = "storage prep job exceeded backoff limit"
			return false, errStoragePrepJobFailed
		}

		return false, nil
	}); err != nil {
		if errors.Is(err, errStoragePrepJobFailed) {
			return buildStoragePrepFailureError(ctx, kubeClient, coreClient, key, failureMessage)
		}
		return fmt.Errorf("wait for storage prep job %s: %w", key.String(), err)
	}

	return nil
}

func buildOpenEBSMayastorPrepJob(params PrepareOpenEBSMayastorParams) *batchv1.Job {
	backoffLimit := int32(0)
	deviceHostPath := hostDevicePath(params.Device)

	return &batchv1.Job{
		ObjectMeta: metav1.ObjectMeta{
			Name:      openEBSMayastorPrepJobName,
			Namespace: openEBSMayastorPrepNamespace,
			Labels: map[string]string{
				"app.kubernetes.io/name":      "waddle-storage-prep",
				"app.kubernetes.io/component": "openebs-mayastor",
				"waddle.cloud/managed":        "true",
			},
		},
		Spec: batchv1.JobSpec{
			BackoffLimit: &backoffLimit,
			Template: corev1.PodTemplateSpec{
				ObjectMeta: metav1.ObjectMeta{
					Labels: map[string]string{
						"app.kubernetes.io/name":      "waddle-storage-prep",
						"app.kubernetes.io/component": "openebs-mayastor",
					},
				},
				Spec: corev1.PodSpec{
					RestartPolicy: corev1.RestartPolicyNever,
					NodeSelector: map[string]string{
						"kubernetes.io/hostname": strings.TrimSpace(params.NodeName),
					},
					Tolerations: []corev1.Toleration{
						{
							Key:      "node-role.kubernetes.io/control-plane",
							Operator: corev1.TolerationOpExists,
							Effect:   corev1.TaintEffectNoSchedule,
						},
						{
							Key:      "node-role.kubernetes.io/control-plane",
							Operator: corev1.TolerationOpExists,
							Effect:   corev1.TaintEffectNoExecute,
						},
						{
							Key:      "node-role.kubernetes.io/master",
							Operator: corev1.TolerationOpExists,
							Effect:   corev1.TaintEffectNoSchedule,
						},
						{
							Key:      "node-role.kubernetes.io/master",
							Operator: corev1.TolerationOpExists,
							Effect:   corev1.TaintEffectNoExecute,
						},
					},
					Containers: []corev1.Container{
						{
							Name:            "prepare-mayastor",
							Image:           openEBSLinuxUtilsImage,
							ImagePullPolicy: corev1.PullIfNotPresent,
							Command:         []string{"/bin/sh", "-euxc"},
							Args: []string{
								buildOpenEBSMayastorPrepScript(deviceHostPath),
							},
							SecurityContext: &corev1.SecurityContext{
								Privileged: ptrTo(true),
								RunAsUser:  ptrTo(int64(0)),
							},
							VolumeMounts: []corev1.VolumeMount{
								{
									Name:      "host-dev",
									MountPath: "/host-dev",
								},
								{
									Name:      "run-udev",
									MountPath: "/run/udev",
									ReadOnly:  true,
								},
								{
									Name:      "host-proc",
									MountPath: "/host-proc",
									ReadOnly:  true,
								},
								{
									Name:      "host-sys",
									MountPath: "/host-sys",
									ReadOnly:  true,
								},
							},
						},
					},
					Volumes: []corev1.Volume{
						{
							Name: "host-dev",
							VolumeSource: corev1.VolumeSource{
								HostPath: &corev1.HostPathVolumeSource{Path: "/dev"},
							},
						},
						{
							Name: "run-udev",
							VolumeSource: corev1.VolumeSource{
								HostPath: &corev1.HostPathVolumeSource{Path: "/run/udev"},
							},
						},
						{
							Name: "host-proc",
							VolumeSource: corev1.VolumeSource{
								HostPath: &corev1.HostPathVolumeSource{Path: "/proc"},
							},
						},
						{
							Name: "host-sys",
							VolumeSource: corev1.VolumeSource{
								HostPath: &corev1.HostPathVolumeSource{Path: "/sys"},
							},
						},
					},
				},
			},
		},
	}
}

func buildOpenEBSMayastorPrepScript(devicePath string) string {
	quotedDevice := shellQuote(devicePath)

	return fmt.Sprintf(`
DEVICE=%s
REAL_DEVICE="/dev/$(basename "$DEVICE")"
HOST_MOUNT_NS="/host-proc/1/ns/mnt"

if [ ! -b "$DEVICE" ]; then
  echo "block device $DEVICE not found" >&2
  exit 1
fi

if [ ! -e "$HOST_MOUNT_NS" ]; then
  echo "host mount namespace $HOST_MOUNT_NS not found" >&2
  exit 1
fi

if [ ! -d /host-sys/module/nvme_tcp ]; then
  echo "host kernel module nvme_tcp is not loaded; update cluster.talosSchematic to include nvme_tcp support" >&2
  exit 1
fi

if command -v pvs >/dev/null 2>&1; then
  PV_REPORT="$(pvs --noheadings --separator='|' -o pv_name,vg_name 2>/dev/null | tr -d ' ' || true)"
  if printf '%%s\n' "$PV_REPORT" | awk -F'|' -v host="$DEVICE" -v real="$REAL_DEVICE" '$1==host || $1==real {found=1} END{exit found?0:1}'; then
    echo "device $REAL_DEVICE is already a physical volume in another volume group" >&2
    exit 1
  fi
fi

MOUNTED_SOURCE=""
while IFS='|' read -r candidate type; do
  [ -n "$candidate" ] || continue
  case "$type" in
    disk|part) ;;
    *) continue ;;
  esac

  candidate_real="/dev/$(basename "$candidate")"
  if nsenter --mount="$HOST_MOUNT_NS" findmnt -rn -S "$candidate_real" >/dev/null 2>&1; then
    MOUNTED_SOURCE="$candidate_real"
    break
  fi
done <<EOF
$(lsblk -nrpo NAME,TYPE "$DEVICE" | awk '{printf "%%s|%%s\n", $1, $2}')
EOF

if [ -n "$MOUNTED_SOURCE" ]; then
  echo "device $REAL_DEVICE or child $MOUNTED_SOURCE is mounted" >&2
  exit 1
fi

if lsblk -nrpo NAME,TYPE "$DEVICE" | awk '$2=="part"{found=1} END{exit found?0:1}'; then
  echo "device $REAL_DEVICE has inactive partitions; wiping stale state"
fi

wipefs -af "$DEVICE"
if command -v sgdisk >/dev/null 2>&1; then
  sgdisk --zap-all "$DEVICE" || true
fi
if command -v blkdiscard >/dev/null 2>&1; then
  blkdiscard -f "$DEVICE" || true
fi
udevadm settle || true
lsblk -nrpo NAME,TYPE "$DEVICE"
`, quotedDevice)
}

func buildStoragePrepFailureError(
	ctx context.Context,
	kubeClient ctrlclient.Client,
	coreClient kubernetes.Interface,
	key ctrlclient.ObjectKey,
	failureMessage string,
) error {
	base := strings.TrimSpace(failureMessage)
	if base == "" {
		base = "storage prep job failed"
	}
	base = fmt.Sprintf("wait for storage prep job %s: %s", key.String(), base)

	if kubeClient == nil {
		return fmt.Errorf("%s", base)
	}

	var pods corev1.PodList
	if err := kubeClient.List(ctx, &pods, ctrlclient.InNamespace(key.Namespace), ctrlclient.MatchingLabels{"job-name": key.Name}); err != nil {
		return fmt.Errorf("%s (list pods: %v)", base, err)
	}

	if len(pods.Items) == 0 {
		return fmt.Errorf("%s", base)
	}

	sort.Slice(pods.Items, func(i, j int) bool {
		return pods.Items[i].Name < pods.Items[j].Name
	})

	details := make([]string, 0, len(pods.Items))
	for _, pod := range pods.Items {
		logs, err := readPodLogsFn(ctx, coreClient, key.Namespace, pod.Name)
		if err != nil {
			details = append(details, fmt.Sprintf("pod %s logs unavailable: %v", pod.Name, err))
			continue
		}
		if logs == "" {
			details = append(details, fmt.Sprintf("pod %s produced no logs", pod.Name))
			continue
		}
		details = append(details, fmt.Sprintf("pod %s logs:\n%s", pod.Name, logs))
	}

	if len(details) == 0 {
		return fmt.Errorf("%s", base)
	}

	return fmt.Errorf("%s\n%s", base, strings.Join(details, "\n\n"))
}

func hostDevicePath(device string) string {
	device = strings.TrimSpace(device)
	if device == "" {
		return "/host-dev"
	}
	return strings.Replace(device, "/dev/", "/host-dev/", 1)
}

func shellQuote(value string) string {
	return "'" + strings.ReplaceAll(value, "'", `'"'"'`) + "'"
}

func newStorageScheme() *runtime.Scheme {
	scheme := runtime.NewScheme()
	_ = batchv1.AddToScheme(scheme)
	_ = corev1.AddToScheme(scheme)
	return scheme
}

func ptrTo[T any](value T) *T {
	return &value
}
