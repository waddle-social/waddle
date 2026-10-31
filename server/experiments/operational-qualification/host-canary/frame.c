#include "probe.h"

#include <inttypes.h>
#include <stdio.h>
#include <string.h>

static const char *state_name(enum observation_state state) {
    switch (state) {
    case UNAVAILABLE:
        return "unavailable";
    case OBSERVED:
        return "observed";
    case FAILED:
        return "failed";
    }
    return NULL;
}

bool format_frame(const struct probe_report *r, char *out, size_t capacity, size_t *length) {
    *length = 0;
    const char *states[] = {state_name(r->status_state),  state_name(r->uid_map_state),
                            state_name(r->gid_map_state), state_name(r->namespace_state),
                            state_name(r->mount_state),   state_name(r->fd_state),
                            state_name(r->cgroup_state),  state_name(r->fsize_state)};
    for (size_t i = 0; i < sizeof(states) / sizeof(states[0]); ++i) {
        if (states[i] == NULL) {
            return false;
        }
    }
    char payload[FRAME_BYTES - 64];
    int size = snprintf(
        payload, sizeof(payload),
        "{\"schema_version\":1,\"component\":\"static_self_inspection\","
        "\"evidence_origin\":\"untrusted_probe_output\","
        "\"controller\":\"unavailable\",\"host_confinement\":\"unverified\","
        "\"qemu_guest_isolation\":\"unverified\","
        "\"requested\":{\"namespaces\":[\"user\",\"pid\",\"mnt\",\"net\",\"ipc\","
        "\"uts\",\"cgroup\"],\"capabilities\":\"none\",\"no_new_privileges\":true,"
        "\"uid_gid_map\":\"nested_zero_to_parent_zero\","
        "\"supplementary_groups\":\"externally_verified_vector\",\"mount_profile\":\"probe_v1\","
        "\"descriptors\":\"null_pipe_pipe\",\"limit_fsize\":65536},"
        "\"observed\":{"
        "\"status\":{\"state\":\"%s\",\"uid\":[%" PRIu64 ",%" PRIu64 ",%" PRIu64 ",%" PRIu64
        "],\"gid\":[%" PRIu64 ",%" PRIu64 ",%" PRIu64 ",%" PRIu64 "],\"caps\":[%" PRIu64 ",%" PRIu64
        ",%" PRIu64 ",%" PRIu64 ",%" PRIu64 "],\"no_new_privileges\":%" PRIu64
        ",\"supplementary_groups\":%" PRIu64 "},"
        "\"uid_map\":{\"state\":\"%s\",\"inside\":%" PRIu64 ",\"outside\":%" PRIu64
        ",\"length\":%" PRIu64 "},"
        "\"gid_map\":{\"state\":\"%s\",\"inside\":%" PRIu64 ",\"outside\":%" PRIu64
        ",\"length\":%" PRIu64 "},"
        "\"namespaces\":{\"state\":\"%s\",\"inodes\":[%" PRIu64 ",%" PRIu64 ",%" PRIu64 ",%" PRIu64
        ",%" PRIu64 ",%" PRIu64 ",%" PRIu64 "]},"
        "\"mounts\":{\"state\":\"%s\",\"count\":%" PRIu64 "},"
        "\"descriptors\":{\"state\":\"%s\",\"count\":%" PRIu64 ",\"stdin_device_major\":%" PRIu64
        ",\"stdin_device_minor\":%" PRIu64 ",\"stdout_inode\":%" PRIu64 ",\"stderr_inode\":%" PRIu64
        "},"
        "\"cgroup_namespace_membership\":{\"state\":\"%s\"},"
        "\"limit_fsize\":{\"state\":\"%s\",\"soft\":%" PRIu64 ",\"hard\":%" PRIu64 "}},"
        "\"unavailable\":[\"trusted_service_baseline\",\"namespace_comparison\",\"external_group_"
        "identity\","
        "\"mount_source_identity\",\"runtime_artifact_closure\",\"device_inventory\","
        "\"loader_environment\",\"further_userns_disabled\","
        "\"effective_cgroup_limits\",\"cgroup_process_membership\",\"sentinel_controls\","
        "\"unit_identity\",\"cleanup\",\"controller_deadlines\",\"journal_bounds\"]}",
        states[0], r->status.uid[0], r->status.uid[1], r->status.uid[2], r->status.uid[3],
        r->status.gid[0], r->status.gid[1], r->status.gid[2], r->status.gid[3], r->status.caps[0],
        r->status.caps[1], r->status.caps[2], r->status.caps[3], r->status.caps[4],
        r->status.no_new_privileges, r->status.supplementary_groups, states[1], r->uid_map.inside,
        r->uid_map.outside, r->uid_map.length, states[2], r->gid_map.inside, r->gid_map.outside,
        r->gid_map.length, states[3], r->namespace_inodes[0], r->namespace_inodes[1],
        r->namespace_inodes[2], r->namespace_inodes[3], r->namespace_inodes[4],
        r->namespace_inodes[5], r->namespace_inodes[6], states[4], r->mount_count, states[5],
        r->fd_count, r->fds[0].device_major, r->fds[0].device_minor, r->fds[1].inode,
        r->fds[2].inode, states[6], states[7], r->fsize_soft, r->fsize_hard);
    if (size < 0 || (size_t)size >= sizeof(payload)) {
        return false;
    }
    int header = snprintf(out, capacity, "WADDLE_HOST_PROBE_V1 %d\n", size);
    if (header < 0 || (size_t)header >= capacity || (size_t)size + (size_t)header + 1 > capacity ||
        (size_t)size + (size_t)header + 1 > FRAME_BYTES) {
        return false;
    }
    memcpy(out + header, payload, (size_t)size);
    *length = (size_t)header + (size_t)size;
    out[(*length)++] = '\n';
    return true;
}
