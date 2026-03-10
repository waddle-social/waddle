package scaleway

import "testing"

func TestBuildInstallPartitioningSchemaKeepsDataDiskByDefault(t *testing.T) {
	schema, err := buildInstallPartitioningSchema(ProvisionParams{
		PivotOSDisk:   "/dev/nvme0n1",
		PivotDataDisk: "/dev/nvme1n1",
	})
	if err != nil {
		t.Fatalf("buildInstallPartitioningSchema returned error: %v", err)
	}

	if len(schema.Disks) != 2 {
		t.Fatalf("disk count = %d, want 2", len(schema.Disks))
	}
	if schema.Disks[1].Device != "/dev/nvme1n1" {
		t.Fatalf("data disk device = %q, want %q", schema.Disks[1].Device, "/dev/nvme1n1")
	}
	if len(schema.Disks[1].Partitions) != 1 {
		t.Fatalf("data disk partition count = %d, want 1", len(schema.Disks[1].Partitions))
	}
}

func TestBuildInstallPartitioningSchemaSkipsDataDiskForDedicatedRawStorage(t *testing.T) {
	schema, err := buildInstallPartitioningSchema(ProvisionParams{
		PivotOSDisk:              "/dev/nvme0n1",
		PivotDataDisk:            "/dev/nvme1n1",
		SkipDataDiskPartitioning: true,
	})
	if err != nil {
		t.Fatalf("buildInstallPartitioningSchema returned error: %v", err)
	}

	if len(schema.Disks) != 1 {
		t.Fatalf("disk count = %d, want 1", len(schema.Disks))
	}
	if schema.Disks[0].Device != "/dev/nvme0n1" {
		t.Fatalf("os disk device = %q, want %q", schema.Disks[0].Device, "/dev/nvme0n1")
	}
}
