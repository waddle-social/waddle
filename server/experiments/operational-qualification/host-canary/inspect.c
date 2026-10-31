#define _GNU_SOURCE
#include "probe.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/sysmacros.h>
#include <time.h>
#include <unistd.h>

#define PROC_SUPER_MAGIC 0x9fa0

/* Paths are fixed at every call site; never use user input or environment here. */
static enum observation_state read_proc(const char *path, char *out, size_t capacity,
                                        size_t *length) {
    *length = 0;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK);
    if (fd < 0) {
        return UNAVAILABLE;
    }
    struct stat info;
    struct statfs filesystem;
    enum observation_state result = FAILED;
    if (fstat(fd, &info) != 0 || !S_ISREG(info.st_mode) || fstatfs(fd, &filesystem) != 0 ||
        filesystem.f_type != PROC_SUPER_MAGIC) {
        goto done;
    }
    /* Bounded bytes and bounded read attempts. No EOF is treated as truncation. */
    for (unsigned attempt = 0; attempt < 16; ++attempt) {
        ssize_t count = read(fd, out + *length, capacity + 1 - *length);
        if (count == 0) {
            result = OBSERVED;
            break;
        }
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            break;
        }
        *length += (size_t)count;
        if (*length > capacity) {
            break;
        }
    }
done:
    if (close(fd) != 0) {
        result = FAILED;
    }
    return result;
}

static enum observation_state inspect_namespaces(uint64_t *inodes) {
    static const char *const paths[NAMESPACE_COUNT] = {
        "/proc/self/ns/user", "/proc/self/ns/pid", "/proc/self/ns/mnt",   "/proc/self/ns/net",
        "/proc/self/ns/ipc",  "/proc/self/ns/uts", "/proc/self/ns/cgroup"};
    enum observation_state result = OBSERVED;
    for (size_t i = 0; i < NAMESPACE_COUNT; ++i) {
        char target[65];
        ssize_t count = readlink(paths[i], target, sizeof(target));
        if (count < 0) {
            if (result != FAILED) {
                result = UNAVAILABLE;
            }
        } else if ((size_t)count == sizeof(target) ||
                   !parse_namespace(target, (size_t)count, i, &inodes[i])) {
            result = FAILED;
        }
    }
    return result;
}

static enum observation_state inspect_fds(struct probe_report *report) {
    DIR *directory = opendir("/proc/self/fd");
    if (directory == NULL) {
        return UNAVAILABLE;
    }
    struct statfs filesystem;
    int scanner = dirfd(directory);
    enum observation_state result = FAILED;
    if (scanner < 0 || fstatfs(scanner, &filesystem) != 0 ||
        filesystem.f_type != PROC_SUPER_MAGIC) {
        goto done;
    }
    unsigned seen = 0;
    for (unsigned attempt = 0; attempt < 8; ++attempt) {
        errno = 0;
        struct dirent *entry = readdir(directory);
        if (entry == NULL) {
            if (errno == 0 && seen == 7u && validate_fds(report->fds, 3)) {
                result = OBSERVED;
            }
            break;
        }
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        uint64_t number;
        if (!parse_fd_number(entry->d_name, strlen(entry->d_name), &number)) {
            break;
        }
        if (number == (uint64_t)scanner) {
            continue; /* The only collector-owned temporary descriptor. */
        }
        ++report->fd_count;
        if (number > 2 || (seen & (1u << number))) {
            break;
        }
        seen |= 1u << number;
        struct stat info;
        int flags = fcntl((int)number, F_GETFL);
        if (flags < 0 || fstat((int)number, &info) != 0) {
            break;
        }
        struct fd_observation *observed = &report->fds[number];
        observed->number = number;
        observed->inode = info.st_ino;
        observed->device_major = major(info.st_rdev);
        observed->device_minor = minor(info.st_rdev);
        observed->access = classify_fd_access(flags);
        if (S_ISCHR(info.st_mode) && observed->device_major == 1 && observed->device_minor == 3) {
            observed->kind = FD_NULL;
        } else if (S_ISFIFO(info.st_mode)) {
            char target[65];
            ssize_t count = readlinkat(scanner, entry->d_name, target, sizeof(target));
            /* Anonymous pipes only; a named FIFO is an unexpected host path. */
            if (count >= 8 && count < (ssize_t)sizeof(target) && memcmp(target, "pipe:[", 6) == 0 &&
                target[count - 1] == ']') {
                uint64_t inode;
                /* Inodes can exceed INT_MAX, so use the namespace integer parser. */
                char link[80];
                int size = snprintf(link, sizeof(link), "pid:[%.*s]", (int)count - 7, target + 6);
                if (size > 0 && (size_t)size < sizeof(link) &&
                    parse_namespace(link, (size_t)size, 1, &inode) && inode == info.st_ino) {
                    observed->kind = FD_PIPE;
                }
            }
        }
    }
done:
    if (closedir(directory) != 0) {
        result = FAILED;
    }
    return result;
}

void inspect_self(struct probe_report *report) {
    memset(report, 0, sizeof(*report));
    char buffer[MOUNT_BYTES + 1];
    size_t length;
    report->status_state = read_proc("/proc/self/status", buffer, STATUS_BYTES, &length);
    if (report->status_state == OBSERVED && !parse_status(buffer, length, &report->status)) {
        report->status_state = FAILED;
    }
    report->uid_map_state = read_proc("/proc/self/uid_map", buffer, MAP_BYTES, &length);
    if (report->uid_map_state == OBSERVED && !parse_id_map(buffer, length, &report->uid_map)) {
        report->uid_map_state = FAILED;
    }
    report->gid_map_state = read_proc("/proc/self/gid_map", buffer, MAP_BYTES, &length);
    if (report->gid_map_state == OBSERVED && !parse_id_map(buffer, length, &report->gid_map)) {
        report->gid_map_state = FAILED;
    }
    report->namespace_state = inspect_namespaces(report->namespace_inodes);
    report->mount_state = read_proc("/proc/self/mountinfo", buffer, MOUNT_BYTES, &length);
    if (report->mount_state == OBSERVED && !parse_mounts(buffer, length, &report->mount_count)) {
        report->mount_state = FAILED;
    }
    report->cgroup_state = read_proc("/proc/self/cgroup", buffer, CGROUP_BYTES, &length);
    if (report->cgroup_state == OBSERVED && !parse_cgroup(buffer, length)) {
        report->cgroup_state = FAILED;
    }
    struct rlimit limit;
    if (getrlimit(RLIMIT_FSIZE, &limit) != 0) {
        report->fsize_state = UNAVAILABLE;
    } else {
        report->fsize_soft = limit.rlim_cur;
        report->fsize_hard = limit.rlim_max;
        report->fsize_state =
            limit.rlim_cur == FSIZE_BYTES && limit.rlim_max == FSIZE_BYTES ? OBSERVED : FAILED;
    }
    report->fd_state = inspect_fds(report);
}

static bool monotonic_ms(uint64_t *value) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0 || now.tv_sec < 0 ||
        (uint64_t)now.tv_sec > (UINT64_MAX - 999u) / 1000u) {
        return false;
    }
    *value = (uint64_t)now.tv_sec * 1000u + (uint64_t)now.tv_nsec / 1000000u;
    return true;
}

bool write_frame(const char *frame, size_t length) {
    if (length == 0 || length > FRAME_BYTES) {
        return false;
    }
    /* Never write evidence to an inherited file, terminal, socket or journal. */
    struct stat info;
    int flags = fcntl(STDOUT_FILENO, F_GETFL);
    if (classify_fd_access(flags) != FD_WRITE_ONLY || fstat(STDOUT_FILENO, &info) != 0 ||
        !S_ISFIFO(info.st_mode) || fcntl(STDOUT_FILENO, F_SETFL, flags | O_NONBLOCK) != 0) {
        return false;
    }
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = SIG_IGN;
    if (sigemptyset(&action.sa_mask) != 0 || sigaction(SIGPIPE, &action, NULL) != 0) {
        return false;
    }
    uint64_t start;
    if (!monotonic_ms(&start) || start > UINT64_MAX - OUTPUT_DEADLINE_MS) {
        return false;
    }
    uint64_t deadline = start + OUTPUT_DEADLINE_MS;
    size_t sent = 0;
    while (sent < length) {
        uint64_t now;
        if (!monotonic_ms(&now) || now >= deadline) {
            return false;
        }
        struct pollfd output = {.fd = STDOUT_FILENO, .events = POLLOUT};
        int ready = poll(&output, 1, (int)(deadline - now));
        if (ready < 0 && errno == EINTR) {
            continue;
        }
        if (ready <= 0 || output.revents != POLLOUT) {
            return false;
        }
        ssize_t count = write(STDOUT_FILENO, frame + sent, length - sent);
        if (count < 0 && (errno == EINTR || errno == EAGAIN)) {
            continue;
        }
        if (count <= 0) {
            return false;
        }
        sent += (size_t)count;
    }
    return true;
}
