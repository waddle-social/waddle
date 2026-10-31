#ifndef WADDLE_HOST_PROBE_H
#define WADDLE_HOST_PROBE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define STATUS_BYTES 16384u
#define MOUNT_BYTES 32768u
#define MAP_BYTES 256u
#define CGROUP_BYTES 1024u
#define FRAME_BYTES 8192u
#define NAMESPACE_COUNT 7u
#define MAX_GROUPS 64u
#define FSIZE_BYTES 65536u
#define OUTPUT_DEADLINE_MS 1000u

enum observation_state { UNAVAILABLE, OBSERVED, FAILED };

struct status_observation {
    uint64_t uid[4];
    uint64_t gid[4];
    uint64_t caps[5];
    uint64_t no_new_privileges;
    uint64_t supplementary_groups;
};

struct id_map {
    uint64_t inside;
    uint64_t outside;
    uint64_t length;
};

enum fd_kind { FD_OTHER, FD_NULL, FD_PIPE };
enum fd_access { FD_ACCESS_REJECTED, FD_READ_ONLY, FD_WRITE_ONLY };
struct fd_observation {
    uint64_t number;
    enum fd_kind kind;
    uint64_t inode;
    uint64_t device_major;
    uint64_t device_minor;
    enum fd_access access;
};

struct probe_report {
    enum observation_state status_state;
    struct status_observation status;
    enum observation_state uid_map_state;
    struct id_map uid_map;
    enum observation_state gid_map_state;
    struct id_map gid_map;
    enum observation_state namespace_state;
    uint64_t namespace_inodes[NAMESPACE_COUNT];
    enum observation_state mount_state;
    uint64_t mount_count;
    enum observation_state fd_state;
    uint64_t fd_count;
    struct fd_observation fds[3];
    enum observation_state cgroup_state;
    enum observation_state fsize_state;
    uint64_t fsize_soft;
    uint64_t fsize_hard;
};

/* Pure parsing and classification: no OS access. Byte inputs are bounded. */
bool parse_status(const char *data, size_t length, struct status_observation *out);
bool parse_id_map(const char *data, size_t length, struct id_map *out);
bool parse_namespace(const char *data, size_t length, size_t kind, uint64_t *inode);
bool parse_mounts(const char *data, size_t length, uint64_t *count);
bool parse_cgroup(const char *data, size_t length);
bool parse_fd_number(const char *data, size_t length, uint64_t *number);
enum fd_access classify_fd_access(int flags);
bool validate_fds(const struct fd_observation *fds, size_t count);
bool format_frame(const struct probe_report *report, char *out, size_t capacity, size_t *length);

/* OS access is isolated here and is never linked into the fixture test. */
void inspect_self(struct probe_report *report);
bool write_frame(const char *frame, size_t length);

#endif
