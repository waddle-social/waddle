package talos

import (
	"fmt"
	"strings"
	"text/template"
)

const defaultOSDisk = "/dev/nvme0n1"
const defaultDataDisk = "/dev/nvme1n1"

// PivotParams holds parameters for generating the Talos pivot script.
type PivotParams struct {
	TalosVersion   string
	TalosSchematic string
	OSDisk         string
	DataDisk       string
	DebugProvision bool
}

// BuildPivotScript generates a cloud-init compatible script that pivots
// a running Ubuntu system to Talos Linux.
func BuildPivotScript(params PivotParams) string {
	osDisk := params.OSDisk
	if osDisk == "" {
		osDisk = defaultOSDisk
	}
	dataDisk := params.DataDisk
	if dataDisk == "" {
		dataDisk = defaultDataDisk
	}
	imageURL := fmt.Sprintf(
		"https://factory.talos.dev/image/%s/%s/metal-amd64.raw.zst",
		params.TalosSchematic,
		params.TalosVersion,
	)

	tpl := template.Must(template.New("pivot").Parse(`#!/usr/bin/env bash
set -euo pipefail

{{- if .DebugProvision }}
export PS4='+ [$(date -u +%Y-%m-%dT%H:%M:%SZ)] ${BASH_SOURCE##*/}:${LINENO}: '
exec > >(tee -a /var/log/waddle-cloud-pivot.log /dev/console) 2>&1
LAST_STEP="starting"
log_step() {
	LAST_STEP="$1"
	echo "==> ${LAST_STEP}"
	printf '%s\n' "${LAST_STEP}" > /run/waddle-cloud-pivot-step
}
pivot_failed() {
	exit_code=$?
	echo "!! pivot failed step=${LAST_STEP:-unknown} line=${BASH_LINENO[0]:-unknown} cmd=${BASH_COMMAND:-unknown} exit=${exit_code}"
	exit "${exit_code}"
}
trap 'pivot_failed' ERR
set -x
{{- else }}
log_step() {
	echo "==> $1"
}
{{- end }}

TALOS_VERSION="{{ .TalosVersion }}"
TALOS_IMAGE_URL="{{ .ImageURL }}"
OS_DISK="{{ .OSDisk }}"
DATA_DISK="{{ .DataDisk }}"

echo "==> Pivoting to Talos Linux ${TALOS_VERSION}"
echo "    Image: ${TALOS_IMAGE_URL}"
echo "    OS disk: ${OS_DISK}"
echo "    Data disk: ${DATA_DISK}"

# 1. Install dependencies
log_step "install dependencies"
apt-get update -qq && apt-get install -y -qq zstd gdisk efibootmgr

# 2. Stage binaries + shared libs to tmpfs (survives dd of root filesystem)
log_step "stage binaries to tmpfs"
STAGE="/dev/shm/talos-stage"
mkdir -p "${STAGE}"/{bin,lib}

for bin in sgdisk efibootmgr; do
	src="$(command -v "${bin}")"
	cp "${src}" "${STAGE}/bin/"
	ldd "${src}" | awk '/=>/{print $3}' | while read -r lib; do
		cp -n "${lib}" "${STAGE}/lib/" 2>/dev/null || true
	done
done

# 3. Download Talos image to tmpfs
log_step "download talos image"
curl -fSL -o /dev/shm/talos.raw.zst "${TALOS_IMAGE_URL}"

# 4. Clear stale EFI boot variables (prevents boot failures on reinstall)
log_step "clear EFI boot variables"
for entry in /sys/firmware/efi/efivars/Boot0*; do
	[ -e "${entry}" ] || continue
	chattr -i "${entry}" 2>/dev/null || true
	rm -f "${entry}"
done

# 5. Wipe data disk boot signatures (prevents fallback to Ubuntu)
if [ -b "${DATA_DISK}" ]; then
	log_step "wipe data disk signatures"
	wipefs -a "${DATA_DISK}" || true
fi

# 6. Write Talos image to OS disk (root filesystem destroyed after this point)
log_step "write Talos image to OS disk"
zstd -d /dev/shm/talos.raw.zst --stdout | dd of="${OS_DISK}" bs=4M status=progress conv=fsync

# --- Root filesystem is gone. Only staged binaries on /dev/shm survive. ---

# 7. Fix GPT backup header (image is smaller than disk, backup GPT is at wrong offset)
log_step "fix GPT backup header"
LD_LIBRARY_PATH="${STAGE}/lib" "${STAGE}/bin/sgdisk" -e "${OS_DISK}"

# 8. Create EFI boot entry for Talos
log_step "create EFI boot entry"
EFI_PART=$(LD_LIBRARY_PATH="${STAGE}/lib" "${STAGE}/bin/sgdisk" -p "${OS_DISK}" | awk '/EF00/{print $1}')
LD_LIBRARY_PATH="${STAGE}/lib" "${STAGE}/bin/efibootmgr" --create --disk "${OS_DISK}" --part "${EFI_PART}" --label "Talos" --loader '\EFI\BOOT\BOOTX64.EFI'

# 9. Hard reboot via sysrq (systemd is gone, normal reboot won't work)
log_step "reboot into Talos maintenance mode"
echo b > /proc/sysrq-trigger
`))

	var builder strings.Builder
	if err := tpl.Execute(&builder, map[string]any{
		"DebugProvision": params.DebugProvision,
		"TalosVersion":   params.TalosVersion,
		"ImageURL":       imageURL,
		"OSDisk":         osDisk,
		"DataDisk":       dataDisk,
	}); err != nil {
		panic(fmt.Sprintf("render pivot script: %v", err))
	}

	return builder.String()
}

// BuildCloudInit wraps the pivot script in a cloud-config YAML suitable
// for Scaleway bare metal user data.
func BuildCloudInit(params PivotParams) string {
	pivotScript := BuildPivotScript(params)
	outputBlock := ""
	if params.DebugProvision {
		outputBlock = `output:
  all: "| tee -a /var/log/cloud-init-output.log /dev/console"
`
	}

	return fmt.Sprintf(`#cloud-config
%swrite_files:
  - path: /usr/local/bin/talos-pivot.sh
    owner: root:root
    permissions: "0755"
    content: |
%s
runcmd:
  - [ bash, -lc, "/usr/local/bin/talos-pivot.sh" ]
`, outputBlock, indentLines(pivotScript, "      "))
}

func indentLines(s, prefix string) string {
	lines := strings.Split(s, "\n")
	for i := range lines {
		lines[i] = prefix + lines[i]
	}
	return strings.Join(lines, "\n")
}
