#define _GNU_SOURCE
#include "probe.h"

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned assertions;

#define CHECK(expression)                                                                          \
    do {                                                                                           \
        ++assertions;                                                                              \
        if (!(expression)) {                                                                       \
            fprintf(stderr, "fixture assertion failed at line %d\n", __LINE__);                    \
            exit(1);                                                                               \
        }                                                                                          \
    } while (0)

static const char good_status[] = "Name:\tprobe\n"
                                  "Uid:\t0\t0\t0\t0\n"
                                  "Gid:\t0\t0\t0\t0\n"
                                  "Groups:\t\n"
                                  "CapInh:\t0000000000000000\n"
                                  "CapPrm:\t0000000000000000\n"
                                  "CapEff:\t0000000000000000\n"
                                  "CapBnd:\t0000000000000000\n"
                                  "CapAmb:\t0000000000000000\n"
                                  "NoNewPrivs:\t1\n";

static const char good_mounts[] =
    "10 9 0:42 /newroot / ro,nosuid,nodev,noexec - tmpfs tmpfs rw\n"
    "11 10 0:43 / /proc ro,nosuid,nodev,noexec - proc proc rw\n"
    "12 10 0:44 /null /dev/null rw,nosuid,noexec - devtmpfs devtmpfs rw\n"
    "13 10 8:1 /run-owned/probe /probe ro,nosuid,nodev - ext4 /dev/synthetic rw\n";

static void replace(const char *input, const char *needle, const char *replacement, char *out,
                    size_t capacity) {
    const char *position = strstr(input, needle);
    CHECK(position != NULL);
    size_t before = (size_t)(position - input);
    size_t after = strlen(position + strlen(needle));
    size_t inserted = strlen(replacement);
    CHECK(before + inserted + after < capacity);
    memcpy(out, input, before);
    memcpy(out + before, replacement, inserted);
    memcpy(out + before + inserted, position + strlen(needle), after + 1);
}

static void status_tests(void) {
    struct status_observation value;
    CHECK(parse_status(good_status, strlen(good_status), &value));
    CHECK(value.no_new_privileges == 1);
    CHECK(!parse_status(good_status, strlen(good_status) - 1, &value));
    CHECK(!parse_status("", 0, &value));
    char changed[STATUS_BYTES + 2];
    const char *fields[] = {
        "Uid:\t0\t0\t0\t0\n",          "Gid:\t0\t0\t0\t0\n",          "Groups:\t\n",
        "CapInh:\t0000000000000000\n", "CapPrm:\t0000000000000000\n", "CapEff:\t0000000000000000\n",
        "CapBnd:\t0000000000000000\n", "CapAmb:\t0000000000000000\n", "NoNewPrivs:\t1\n"};
    for (size_t i = 0; i < sizeof(fields) / sizeof(fields[0]); ++i) {
        replace(good_status, fields[i], "", changed, sizeof(changed));
        CHECK(!parse_status(changed, strlen(changed), &value));
        int size = snprintf(changed, sizeof(changed), "%s%s", good_status, fields[i]);
        CHECK(size > 0 && (size_t)size < sizeof(changed));
        CHECK(!parse_status(changed, (size_t)size, &value));
    }
    for (size_t i = 3; i < 8; ++i) {
        replace(good_status, fields[i], fields[i], changed, sizeof(changed));
        char *cap = strstr(changed, fields[i]);
        CHECK(cap != NULL);
        cap[strlen(fields[i]) - 2] = '1';
        CHECK(!parse_status(changed, strlen(changed), &value));
    }
    const char *needles[] = {"NoNewPrivs:\t1",
                             "Groups:\t",
                             "Uid:\t0\t0\t0\t0",
                             "Gid:\t0\t0\t0\t0",
                             "CapEff:\t0000000000000000",
                             "CapAmb:\t0000000000000000"};
    const char *replacements[] = {"NoNewPrivs:\t0",
                                  "Groups:\t-1",
                                  "Uid:\t0\t0\t1\t0",
                                  "Gid:\t0\t0\t0\t1",
                                  "CapEff:\t000000000000000g",
                                  "CapAmb:\t00000000000000000"};
    for (size_t i = 0; i < sizeof(needles) / sizeof(needles[0]); ++i) {
        replace(good_status, needles[i], replacements[i], changed, sizeof(changed));
        CHECK(!parse_status(changed, strlen(changed), &value));
    }
    replace(good_status, "Groups:\t",
            "Groups:\t0 65534 65534 65534 65534 65534 65534 65534 65534 65534 65534", changed,
            sizeof(changed));
    CHECK(parse_status(changed, strlen(changed), &value));
    CHECK(value.supplementary_groups == 11);
    char groups[512] = "Groups:\t";
    for (size_t i = 0; i < MAX_GROUPS; ++i) {
        CHECK(strlen(groups) + sizeof("65534 ") < sizeof(groups));
        strcat(groups, "65534 ");
    }
    replace(good_status, "Groups:\t", groups, changed, sizeof(changed));
    CHECK(parse_status(changed, strlen(changed), &value));
    CHECK(value.supplementary_groups == MAX_GROUPS);
    strcat(groups, "65534 ");
    replace(good_status, "Groups:\t", groups, changed, sizeof(changed));
    CHECK(!parse_status(changed, strlen(changed), &value));
    memcpy(changed, good_status, sizeof(good_status));
    changed[2] = '\0';
    CHECK(!parse_status(changed, strlen(good_status), &value));
    memset(changed, 'a', sizeof(changed));
    changed[sizeof(changed) - 1] = '\n';
    CHECK(!parse_status(changed, sizeof(changed), &value));
}

static void map_and_namespace_tests(void) {
    struct id_map map;
    const char valid[] = "         0          0          1\n";
    CHECK(parse_id_map(valid, strlen(valid), &map));
    CHECK(map.outside == 0 && map.inside == 0 && map.length == 1);
    const char *invalid[] = {"",
                             "0 1000 1",
                             "0 1000 1\n",
                             "0 1000 2\n",
                             "1 1000 1\n",
                             "0 1000 1\n1 1001 1\n",
                             "0 4294967295 1\n",
                             "0 -1 1\n",
                             "0 18446744073709551616 1\n",
                             "0 1000 1 extra\n",
                             "0 1000 1\r\n",
                             "0 1000\n"};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        CHECK(!parse_id_map(invalid[i], strlen(invalid[i]), &map));
    }
    const char *names[] = {"user", "pid", "mnt", "net", "ipc", "uts", "cgroup"};
    uint64_t inode;
    for (size_t i = 0; i < NAMESPACE_COUNT; ++i) {
        char text[80];
        int size = snprintf(text, sizeof(text), "%s:[4026531837]", names[i]);
        CHECK(size > 0);
        CHECK(parse_namespace(text, (size_t)size, i, &inode));
        CHECK(inode == 4026531837);
        CHECK(!parse_namespace(text, (size_t)size - 1, i, &inode));
        CHECK(!parse_namespace(text, (size_t)size, (i + 1) % NAMESPACE_COUNT, &inode));
    }
    const char *bad_ns[] = {"",          "pid:[0]",     "pid:[-1]",
                            "pid:[1]\n", "pid:[1]junk", "pid:[18446744073709551616]",
                            "pid:[1\0]"};
    for (size_t i = 0; i < sizeof(bad_ns) / sizeof(bad_ns[0]); ++i) {
        CHECK(!parse_namespace(bad_ns[i], strlen(bad_ns[i]), 1, &inode));
    }
    CHECK(!parse_namespace("pid:[1]", 7, NAMESPACE_COUNT, &inode));
}

static void mount_tests(void) {
    uint64_t count;
    CHECK(parse_mounts(good_mounts, strlen(good_mounts), &count));
    CHECK(count == 4);
    CHECK(!parse_mounts(good_mounts, strlen(good_mounts) - 1, &count));
    CHECK(!parse_mounts("", 0, &count));
    char changed[MOUNT_BYTES + 2];
    const char *targets[] = {"/", "/proc", "/dev/null", "/probe"};
    const char *required[] = {"ro,nosuid,nodev,noexec", "ro,nosuid,nodev,noexec",
                              "rw,nosuid,noexec", "ro,nosuid,nodev"};
    for (size_t i = 0; i < sizeof(required) / sizeof(required[0]); ++i) {
        char needle[128], flags[128];
        int size = snprintf(needle, sizeof(needle), " %s %s ", targets[i], required[i]);
        CHECK(size > 0 && (size_t)size < sizeof(needle));
        size = snprintf(flags, sizeof(flags), " %s %s,relatime ", targets[i], required[i]);
        CHECK(size > 0 && (size_t)size < sizeof(flags));
        replace(good_mounts, needle, flags, changed, sizeof(changed));
        CHECK(parse_mounts(changed, strlen(changed), &count));
        size = snprintf(flags, sizeof(flags), " %s relatime,%s ", targets[i], required[i]);
        CHECK(size > 0 && (size_t)size < sizeof(flags));
        replace(good_mounts, needle, flags, changed, sizeof(changed));
        CHECK(parse_mounts(changed, strlen(changed), &count));
        const char *extra[] = {"relatime,relatime", "strictatime", "noatime", "lazytime", "ro"};
        for (size_t j = 0; j < sizeof(extra) / sizeof(extra[0]); ++j) {
            size = snprintf(flags, sizeof(flags), " %s %s,%s ", targets[i], required[i], extra[j]);
            CHECK(size > 0 && (size_t)size < sizeof(flags));
            replace(good_mounts, needle, flags, changed, sizeof(changed));
            CHECK(!parse_mounts(changed, strlen(changed), &count));
        }
        const char *start = required[i];
        while (start != NULL) {
            const char *end = strchr(start, ',');
            char weakened[128];
            /* Replace exactly one required flag with optional relatime. */
            int prefix = (int)(start - required[i]);
            size = snprintf(weakened, sizeof(weakened), "%.*srelatime%s", prefix, required[i],
                            end == NULL ? "" : end);
            CHECK(size > 0 && (size_t)size < sizeof(weakened));
            size = snprintf(flags, sizeof(flags), " %s %s ", targets[i], weakened);
            CHECK(size > 0 && (size_t)size < sizeof(flags));
            replace(good_mounts, needle, flags, changed, sizeof(changed));
            CHECK(!parse_mounts(changed, strlen(changed), &count));
            start = end == NULL ? NULL : end + 1;
        }
    }
    const char *needles[] = {" /newroot / ",
                             " /proc ",
                             " /dev/null ",
                             " /probe ",
                             "ro,nosuid,nodev,noexec",
                             "ro,nosuid,nodev,noexec",
                             "rw,nosuid,noexec",
                             " - proc",
                             " - tmpfs",
                             "12 10",
                             "13 10",
                             "11 10",
                             " /null ",
                             " /run-owned/probe ",
                             "0:42"};
    const char *replacements[] = {" / / ",
                                  " /sys ",
                                  " /dev/kvm ",
                                  " /run ",
                                  "rw,nosuid,nodev,noexec",
                                  "ro,nosuid,nodev,noexec shared:1",
                                  "rw,nosuid,nodev,noexec",
                                  " - ext4",
                                  " - ext4",
                                  "11 10",
                                  "13 9",
                                  "11 11",
                                  " /secret ",
                                  " /run\\040owned/probe ",
                                  "-1:42"};
    for (size_t i = 0; i < sizeof(needles) / sizeof(needles[0]); ++i) {
        replace(good_mounts, needles[i], replacements[i], changed, sizeof(changed));
        CHECK(!parse_mounts(changed, strlen(changed), &count));
    }
    const char *line = good_mounts;
    for (size_t i = 0; i < 4; ++i) {
        const char *end = strchr(line, '\n');
        CHECK(end != NULL);
        size_t before = (size_t)(line - good_mounts);
        memcpy(changed, good_mounts, before);
        strcpy(changed + before, end + 1);
        CHECK(!parse_mounts(changed, strlen(changed), &count));
        line = end + 1;
    }
    int size = snprintf(changed, sizeof(changed), "%s%s", good_mounts, good_mounts);
    CHECK(size > 0);
    CHECK(!parse_mounts(changed, (size_t)size, &count));
    memset(changed, 'a', sizeof(changed));
    changed[sizeof(changed) - 1] = '\n';
    CHECK(!parse_mounts(changed, sizeof(changed), &count));
}

static void fd_and_cgroup_tests(void) {
    struct fd_observation fds[] = {
        {.number = 0,
         .kind = FD_NULL,
         .device_major = 1,
         .device_minor = 3,
         .access = FD_READ_ONLY},
        {.number = 1, .kind = FD_PIPE, .inode = 100, .access = FD_WRITE_ONLY},
        {.number = 2, .kind = FD_PIPE, .inode = 101, .access = FD_WRITE_ONLY}};
    CHECK(validate_fds(fds, 3));
    CHECK(!validate_fds(fds, 0));
    CHECK(!validate_fds(fds, 2));
    CHECK(!validate_fds(fds, 4));
    const struct {
        int flags;
        enum fd_access expected;
    } access_cases[] = {
        {O_RDONLY, FD_READ_ONLY},
        {O_WRONLY, FD_WRITE_ONLY},
        {O_RDONLY | O_NONBLOCK, FD_READ_ONLY},
        {O_WRONLY | O_NONBLOCK, FD_WRITE_ONLY},
        {O_RDWR, FD_ACCESS_REJECTED},
        {O_RDWR | O_NONBLOCK, FD_ACCESS_REJECTED},
        {O_ACCMODE, FD_ACCESS_REJECTED},
        {-1, FD_ACCESS_REJECTED},
        {O_PATH | O_RDONLY, FD_ACCESS_REJECTED},
        {O_PATH | O_WRONLY, FD_ACCESS_REJECTED},
        {O_PATH | O_RDWR, FD_ACCESS_REJECTED},
        {O_PATH | O_ACCMODE, FD_ACCESS_REJECTED},
        {O_PATH | O_NONBLOCK | O_NOFOLLOW, FD_ACCESS_REJECTED},
    };
    for (size_t i = 0; i < sizeof(access_cases) / sizeof(access_cases[0]); ++i) {
        enum fd_access access = classify_fd_access(access_cases[i].flags);
        CHECK(access == access_cases[i].expected);
        for (size_t fd = 0; fd < 3; ++fd) {
            enum fd_access saved = fds[fd].access;
            fds[fd].access = access;
            CHECK(validate_fds(fds, 3) == (access == saved));
            fds[fd].access = saved;
        }
    }
    for (size_t i = 0; i < 3; ++i) {
        enum fd_kind saved = fds[i].kind;
        fds[i].kind = FD_OTHER;
        CHECK(!validate_fds(fds, 3));
        fds[i].kind = saved;
        fds[i].number = 9;
        CHECK(!validate_fds(fds, 3));
        fds[i].number = i;
    }
    fds[0].device_minor = 5;
    CHECK(!validate_fds(fds, 3));
    fds[0].device_minor = 3;
    fds[1].access = FD_READ_ONLY;
    CHECK(!validate_fds(fds, 3));
    fds[1].access = FD_WRITE_ONLY;
    fds[2].inode = 100;
    CHECK(!validate_fds(fds, 3));
    fds[2].inode = 101;
    fds[2].number = 1;
    CHECK(!validate_fds(fds, 3));
    uint64_t number;
    CHECK(parse_fd_number("0", 1, &number) && number == 0);
    const char *bad_fd[] = {"", "-1", "03", "1;sh", "2147483648", "1\n"};
    for (size_t i = 0; i < sizeof(bad_fd) / sizeof(bad_fd[0]); ++i) {
        CHECK(!parse_fd_number(bad_fd[i], strlen(bad_fd[i]), &number));
    }
    CHECK(parse_cgroup("0::/\n", 5));
    const char *bad_cgroup[] = {"",          "0::/",     "0::/user.slice/run\n", "0::/\n0::/\n",
                                "1:cpu:/\n", "0::/../\n"};
    for (size_t i = 0; i < sizeof(bad_cgroup) / sizeof(bad_cgroup[0]); ++i) {
        CHECK(!parse_cgroup(bad_cgroup[i], strlen(bad_cgroup[i])));
    }
}

static void frame_tests(void) {
    struct probe_report report = {0};
    char frame[FRAME_BYTES + 1];
    size_t length;
    CHECK(format_frame(&report, frame, FRAME_BYTES, &length));
    CHECK(length > 0 && length <= FRAME_BYTES);
    frame[length] = '\0';
    CHECK(strstr(frame, "\"controller\":\"unavailable\"") != NULL);
    CHECK(strstr(frame, "\"host_confinement\":\"unverified\"") != NULL);
    CHECK(strstr(frame, "\"qemu_guest_isolation\":\"unverified\"") != NULL);
    const char *payload = strchr(frame, '\n');
    CHECK(payload != NULL);
    unsigned encoded_length = 0;
    CHECK(sscanf(frame, "WADDLE_HOST_PROBE_V1 %u", &encoded_length) == 1);
    CHECK(encoded_length + (size_t)(payload - frame) + 2 == length);
    CHECK(frame[length - 1] == '\n');
    CHECK(!format_frame(&report, frame, 0, &length));
    CHECK(!format_frame(&report, frame, 100, &length));
    report.status_state = (enum observation_state)99;
    CHECK(!format_frame(&report, frame, FRAME_BYTES, &length));
    report.status_state = OBSERVED;
    CHECK(parse_status(good_status, strlen(good_status), &report.status));
    report.uid_map_state = report.gid_map_state = OBSERVED;
    report.uid_map = report.gid_map = (struct id_map){0, 0, 1};
    report.namespace_state = OBSERVED;
    for (size_t i = 0; i < NAMESPACE_COUNT; ++i) {
        report.namespace_inodes[i] = i + 100;
    }
    report.mount_state = OBSERVED;
    report.mount_count = 4;
    report.fd_state = OBSERVED;
    report.fd_count = 3;
    report.fds[0].device_major = 1;
    report.fds[0].device_minor = 3;
    report.fds[1].inode = 1000;
    report.fds[2].inode = 1001;
    report.cgroup_state = report.fsize_state = OBSERVED;
    report.fsize_soft = report.fsize_hard = FSIZE_BYTES;
    CHECK(format_frame(&report, frame, FRAME_BYTES, &length));
    /* Export only a synthetic fixture for the independent frame decoder. */
    CHECK(fwrite(frame, 1, length, stdout) == length);
}

int main(void) {
    status_tests();
    map_and_namespace_tests();
    mount_tests();
    fd_and_cgroup_tests();
    frame_tests();
    fprintf(stderr, "%u pure fixture assertions passed\n", assertions);
    return 0;
}
