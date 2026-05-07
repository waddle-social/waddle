package scaleway

import (
	"fmt"
	"strings"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	"github.com/scaleway/scaleway-sdk-go/scw"
)

func buildInstallPartitioningSchema(params ProvisionParams) (*baremetal.Schema, error) {
	osDisk := strings.TrimSpace(params.PivotOSDisk)
	if osDisk == "" {
		return nil, fmt.Errorf("pivot_os_disk is required for custom partitioning")
	}

	dataDisk := strings.TrimSpace(params.PivotDataDisk)
	if dataDisk == osDisk {
		return nil, fmt.Errorf("pivot_data_disk must differ from pivot_os_disk")
	}

	const (
		uefiSizeBytes = 512 * 1024 * 1024
		swapSizeBytes = 4 * 1024 * 1024 * 1024
		bootSizeBytes = 512 * 1024 * 1024
		rootSizeBytes = 1018839433216
	)

	disks := []*baremetal.SchemaDisk{
		{
			Device: osDisk,
			Partitions: []*baremetal.SchemaPartition{
				{Label: baremetal.SchemaPartitionLabelUefi, Number: 1, Size: scw.Size(uefiSizeBytes)},
				{Label: baremetal.SchemaPartitionLabelSwap, Number: 2, Size: scw.Size(swapSizeBytes)},
				{Label: baremetal.SchemaPartitionLabelBoot, Number: 3, Size: scw.Size(bootSizeBytes)},
				{Label: baremetal.SchemaPartitionLabelRoot, Number: 4, Size: scw.Size(rootSizeBytes)},
			},
		},
	}

	filesystems := []*baremetal.SchemaFilesystem{
		{Device: partitionDeviceForInstall(osDisk, 1), Format: baremetal.SchemaFilesystemFormatFat32, Mountpoint: "/boot/efi"},
		{Device: partitionDeviceForInstall(osDisk, 3), Format: baremetal.SchemaFilesystemFormatExt4, Mountpoint: "/boot"},
		{Device: partitionDeviceForInstall(osDisk, 4), Format: baremetal.SchemaFilesystemFormatExt4, Mountpoint: "/"},
	}

	if dataDisk != "" && !params.SkipDataDiskPartitioning {
		disks = append(disks, &baremetal.SchemaDisk{
			Device: dataDisk,
			Partitions: []*baremetal.SchemaPartition{
				{Label: baremetal.SchemaPartitionLabelData, Number: 1, Size: scw.Size(rootSizeBytes)},
			},
		})
	}

	return &baremetal.Schema{
		Disks:       disks,
		Filesystems: filesystems,
		Raids:       []*baremetal.SchemaRAID{},
		Zfs: &baremetal.SchemaZFS{
			Pools: []*baremetal.SchemaPool{},
		},
	}, nil
}

func partitionDeviceForInstall(disk string, number uint32) string {
	if strings.HasPrefix(disk, "/dev/nvme") || strings.HasPrefix(disk, "/dev/mmcblk") {
		return fmt.Sprintf("%sp%d", disk, number)
	}
	return fmt.Sprintf("%s%d", disk, number)
}
