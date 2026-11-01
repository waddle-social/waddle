#define _GNU_SOURCE
#include "probe.h"

#include <fcntl.h>
#include <limits.h>
#include <string.h>

struct span {
    const char *data;
    size_t length;
};

static bool equal(struct span value, const char *literal) {
    size_t length = strlen(literal);
    return value.length == length && memcmp(value.data, literal, length) == 0;
}

static bool valid_text(const char *data, size_t length, size_t maximum) {
    return length > 0 && length <= maximum && data[length - 1] == '\n' &&
           memchr(data, '\0', length) == NULL && memchr(data, '\r', length) == NULL;
}

static bool number(struct span token, unsigned base, uint64_t *out) {
    if (token.length == 0 || token.length > 20) {
        return false;
    }
    uint64_t value = 0;
    for (size_t i = 0; i < token.length; ++i) {
        unsigned char byte = (unsigned char)token.data[i];
        unsigned digit;
        if (byte >= '0' && byte <= '9') {
            digit = byte - '0';
        } else if (base == 16 && byte >= 'a' && byte <= 'f') {
            digit = byte - 'a' + 10;
        } else {
            return false;
        }
        if (digit >= base || value > (UINT64_MAX - digit) / base) {
            return false;
        }
        value = value * base + digit;
    }
    *out = value;
    return true;
}

bool parse_fd_number(const char *data, size_t length, uint64_t *out) {
    return !(length > 1 && data[0] == '0') && number((struct span){data, length}, 10, out) &&
           *out <= INT_MAX;
}

static bool tokens(struct span line, struct span *out, size_t capacity, size_t *count) {
    *count = 0;
    size_t offset = 0;
    while (offset < line.length) {
        while (offset < line.length && (line.data[offset] == ' ' || line.data[offset] == '\t')) {
            ++offset;
        }
        if (offset == line.length) {
            break;
        }
        if (*count == capacity) {
            return false;
        }
        size_t start = offset;
        while (offset < line.length && line.data[offset] != ' ' && line.data[offset] != '\t') {
            ++offset;
        }
        out[(*count)++] = (struct span){line.data + start, offset - start};
    }
    return true;
}

static struct span next_line(const char *data, size_t length, size_t *offset) {
    size_t start = *offset;
    while (*offset < length && data[*offset] != '\n') {
        ++*offset;
    }
    struct span line = {data + start, *offset - start};
    if (*offset < length) {
        ++*offset;
    }
    return line;
}

bool parse_status(const char *data, size_t length, struct status_observation *out) {
    memset(out, 0, sizeof(*out));
    if (!valid_text(data, length, STATUS_BYTES)) {
        return false;
    }
    static const char *const keys[] = {"Uid:",    "Gid:",    "Groups:", "CapInh:",    "CapPrm:",
                                       "CapEff:", "CapBnd:", "CapAmb:", "NoNewPrivs:"};
    unsigned seen = 0;
    size_t offset = 0;
    while (offset < length) {
        struct span line = next_line(data, length, &offset);
        const char *colon = memchr(line.data, ':', line.length);
        if (colon == NULL || line.length > 4096) {
            return false;
        }
        struct span key = {line.data, (size_t)(colon - line.data) + 1};
        size_t kind = 0;
        while (kind < 9 && !equal(key, keys[kind])) {
            ++kind;
        }
        if (kind == 9) {
            continue; /* Other kernel status fields are not emitted or trusted. */
        }
        if (seen & (1u << kind)) {
            return false;
        }
        seen |= 1u << kind;
        struct span value = {colon + 1, line.length - key.length};
        if (kind == 2) {
            struct span groups[MAX_GROUPS];
            size_t count;
            if (!tokens(value, groups, MAX_GROUPS, &count)) {
                return false;
            }
            for (size_t i = 0; i < count; ++i) {
                uint64_t group;
                if (!number(groups[i], 10, &group) || group >= UINT32_MAX) {
                    return false;
                }
            }
            /* Namespace-local IDs may alias; only a trusted external observer
             * can authenticate the host group vector. */
            out->supplementary_groups = count;
            continue;
        }
        struct span fields[5];
        size_t count;
        if (!tokens(value, fields, 5, &count)) {
            return false;
        }
        if (kind < 2) {
            uint64_t *ids = kind == 0 ? out->uid : out->gid;
            if (count != 4) {
                return false;
            }
            for (size_t i = 0; i < 4; ++i) {
                if (!number(fields[i], 10, &ids[i]) || ids[i] != 0) {
                    return false;
                }
            }
        } else if (kind < 8) {
            if (count != 1 || fields[0].length != 16 ||
                !number(fields[0], 16, &out->caps[kind - 3]) || out->caps[kind - 3] != 0) {
                return false;
            }
        } else if (count != 1 || !number(fields[0], 10, &out->no_new_privileges) ||
                   out->no_new_privileges != 1) {
            return false;
        }
    }
    return seen == 511u;
}

bool parse_id_map(const char *data, size_t length, struct id_map *out) {
    memset(out, 0, sizeof(*out));
    if (!valid_text(data, length, MAP_BYTES) || memchr(data, '\n', length - 1) != NULL) {
        return false;
    }
    struct span fields[3];
    size_t count;
    return tokens((struct span){data, length - 1}, fields, 3, &count) && count == 3 &&
           number(fields[0], 10, &out->inside) && number(fields[1], 10, &out->outside) &&
           number(fields[2], 10, &out->length) && out->inside == 0 && out->outside == 0 &&
           out->length == 1;
}

bool parse_namespace(const char *data, size_t length, size_t kind, uint64_t *inode) {
    static const char *const names[NAMESPACE_COUNT] = {"user", "pid", "mnt",   "net",
                                                       "ipc",  "uts", "cgroup"};
    *inode = 0;
    if (kind >= NAMESPACE_COUNT || length > 64) {
        return false;
    }
    size_t prefix = strlen(names[kind]);
    if (length < prefix + 4 || memcmp(data, names[kind], prefix) != 0 || data[prefix] != ':' ||
        data[prefix + 1] != '[' || data[length - 1] != ']') {
        return false;
    }
    return number((struct span){data + prefix + 2, length - prefix - 3}, 10, inode) && *inode != 0;
}

static bool mount_options(struct span options, unsigned required) {
    static const char *const names[] = {"ro", "rw", "nosuid", "nodev", "noexec", "relatime"};
    unsigned seen = 0;
    size_t offset = 0;
    while (offset < options.length) {
        size_t start = offset;
        while (offset < options.length && options.data[offset] != ',') {
            ++offset;
        }
        struct span option = {options.data + start, offset - start};
        size_t kind = 0;
        while (kind < 6 && !equal(option, names[kind])) {
            ++kind;
        }
        if (kind == 6 || (seen & (1u << kind))) {
            return false;
        }
        seen |= 1u << kind;
        if (offset < options.length) {
            ++offset;
            if (offset == options.length) {
                return false;
            }
        }
    }
    return (seen & ~32u) == required;
}

static bool mount_device(struct span token) {
    const char *colon = memchr(token.data, ':', token.length);
    uint64_t major;
    uint64_t minor;
    return colon != NULL &&
           number((struct span){token.data, (size_t)(colon - token.data)}, 10, &major) &&
           number((struct span){colon + 1, token.length - (size_t)(colon - token.data) - 1}, 10,
                  &minor) &&
           major <= UINT32_MAX && minor <= UINT32_MAX;
}

bool parse_mounts(const char *data, size_t length, uint64_t *count) {
    *count = 0;
    if (!valid_text(data, length, MOUNT_BYTES)) {
        return false;
    }
    static const char *const targets[] = {"/", "/proc", "/dev/null", "/probe"};
    static const unsigned options[] = {29u, 29u, 22u, 13u};
    uint64_t ids[4] = {0};
    uint64_t parents[4] = {0};
    unsigned seen = 0;
    size_t offset = 0;
    while (offset < length) {
        struct span line = next_line(data, length, &offset);
        struct span fields[10];
        size_t fields_count;
        if (++*count > 4 || !tokens(line, fields, 10, &fields_count) || fields_count != 10 ||
            !equal(fields[6], "-") || !mount_device(fields[2])) {
            return false;
        }
        size_t kind = 0;
        while (kind < 4 && !equal(fields[4], targets[kind])) {
            ++kind;
        }
        if (kind == 4 || (seen & (1u << kind)) || !number(fields[0], 10, &ids[kind]) ||
            ids[kind] == 0 || !number(fields[1], 10, &parents[kind]) || parents[kind] == 0 ||
            fields[3].length == 0 || fields[3].data[0] != '/' ||
            memchr(fields[3].data, '\\', fields[3].length) != NULL ||
            !mount_options(fields[5], options[kind])) {
            return false;
        }
        seen |= 1u << kind;
        if ((kind == 0 && (!equal(fields[7], "tmpfs") || !equal(fields[3], "/newroot"))) ||
            (kind == 1 && (!equal(fields[7], "proc") || !equal(fields[3], "/"))) ||
            (kind == 2 && (!equal(fields[3], "/null") ||
                           !(equal(fields[7], "devtmpfs") || equal(fields[7], "tmpfs")))) ||
            (kind == 3 && !(equal(fields[7], "tmpfs") || equal(fields[7], "ext4") ||
                            equal(fields[7], "btrfs") || equal(fields[7], "xfs")))) {
            return false;
        }
    }
    if (seen != 15u) {
        return false;
    }
    for (size_t i = 0; i < 4; ++i) {
        if ((i != 0 && parents[i] != ids[0]) || parents[i] == ids[i]) {
            return false;
        }
        for (size_t j = 0; j < i; ++j) {
            if (ids[i] == ids[j]) {
                return false;
            }
        }
    }
    return true;
}

bool parse_cgroup(const char *data, size_t length) {
    return length == 5 && memcmp(data, "0::/\n", 5) == 0;
}

enum fd_access classify_fd_access(int flags) {
    /* O_PATH has zero access-mode bits but cannot perform read or write I/O. */
    if (flags < 0 || (flags & O_PATH) != 0) {
        return FD_ACCESS_REJECTED;
    }
    switch (flags & O_ACCMODE) {
    case O_RDONLY:
        return FD_READ_ONLY;
    case O_WRONLY:
        return FD_WRITE_ONLY;
    default:
        return FD_ACCESS_REJECTED;
    }
}

bool validate_fds(const struct fd_observation *fds, size_t count) {
    if (count != 3) {
        return false;
    }
    const struct fd_observation *ordered[3] = {NULL};
    for (size_t i = 0; i < count; ++i) {
        if (fds[i].number > 2 || ordered[fds[i].number] != NULL) {
            return false;
        }
        ordered[fds[i].number] = &fds[i];
    }
    return ordered[0]->kind == FD_NULL && ordered[0]->device_major == 1 &&
           ordered[0]->device_minor == 3 && ordered[0]->access == FD_READ_ONLY &&
           ordered[1]->kind == FD_PIPE && ordered[1]->access == FD_WRITE_ONLY &&
           ordered[2]->kind == FD_PIPE && ordered[2]->access == FD_WRITE_ONLY &&
           ordered[1]->inode != 0 && ordered[2]->inode != 0 &&
           ordered[1]->inode != ordered[2]->inode;
}
