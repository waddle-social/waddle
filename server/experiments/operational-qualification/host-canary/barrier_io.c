#define _GNU_SOURCE
#include "barrier.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <time.h>
#include <unistd.h>

#define PROC_SUPER_MAGIC 0x9fa0
#define RELEASE_FD 3

static bool channel(int scanner, int fd, const char *name, char *link,
                    struct barrier_channel *out) {
    struct stat info;
    int flags = fcntl(fd, F_GETFL);
    ssize_t length = readlinkat(scanner, name, link, 65);
    if (flags < 0 || fstat(fd, &info) != 0 || length < 0 || length >= 65) {
        return false;
    }
    *out = (struct barrier_channel){.fifo = S_ISFIFO(info.st_mode),
                                    .flags = flags,
                                    .device = info.st_dev,
                                    .inode = info.st_ino,
                                    .link = link,
                                    .link_length = (size_t)length};
    return true;
}

static bool validate_channels(int *input_flags, int *output_flags) {
    int scanner =
        open("/proc/self/fd", O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK);
    if (scanner < 0) {
        return false;
    }
    struct statfs filesystem;
    char input_link[65], output_link[65];
    struct barrier_channel input, output;
    bool valid = fstatfs(scanner, &filesystem) == 0 && filesystem.f_type == PROC_SUPER_MAGIC &&
                 channel(scanner, RELEASE_FD, "3", input_link, &input) &&
                 channel(scanner, STDERR_FILENO, "2", output_link, &output) &&
                 barrier_channels_valid(&input, &output);
    if (valid) {
        *input_flags = input.flags;
        *output_flags = output.flags;
    }
    /* No validation scratch descriptor survives the readiness marker. */
    return close(scanner) == 0 && valid;
}

static bool monotonic_ms(uint64_t *value) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0 || now.tv_sec < 0 || now.tv_nsec < 0 ||
        now.tv_nsec >= 1000000000L || (uint64_t)now.tv_sec > (UINT64_MAX - 999u) / 1000u) {
        return false;
    }
    *value = (uint64_t)now.tv_sec * 1000u + (uint64_t)now.tv_nsec / 1000000u;
    return true;
}

static int time_left(struct barrier_state *state, bool begin) {
    uint64_t now = 0;
    bool clock_ok = monotonic_ms(&now);
    return begin ? barrier_begin_attempt(state, clock_ok, now)
                 : barrier_time_left(state, clock_ok, now);
}

static bool exchange(void) {
    uint64_t start = 0;
    struct barrier_state state;
    bool clock_ok = monotonic_ms(&start);
    if (!barrier_start(&state, clock_ok, start)) {
        return false;
    }
    while (state.phase == BARRIER_READY_WRITE || state.phase == BARRIER_RELEASE_READ) {
        int remaining = time_left(&state, true);
        if (remaining == 0) {
            return false;
        }
        bool writing = state.phase == BARRIER_READY_WRITE;
        struct pollfd pipe = {.fd = writing ? STDERR_FILENO : RELEASE_FD,
                              .events = writing ? POLLOUT : POLLIN};
        int ready = poll(&pipe, 1, remaining);
        int poll_error = errno;
        bool perform_io;
        if (time_left(&state, false) == 0 ||
            !barrier_poll_event(&state, ready, pipe.revents, poll_error, &perform_io)) {
            return false;
        }
        if (!perform_io) {
            continue;
        }
        char bytes[2];
        ssize_t count = writing ? write(STDERR_FILENO, BARRIER_READY + state.sent,
                                        BARRIER_READY_BYTES - state.sent)
                                : read(RELEASE_FD, bytes, sizeof(bytes));
        int io_error = errno;
        if (time_left(&state, false) == 0 ||
            !(writing ? barrier_write_event(&state, count, io_error)
                      : barrier_read_event(&state, bytes, count, io_error))) {
            return false;
        }
    }
    return state.phase == BARRIER_ACCEPTED;
}

bool await_controller_release(void) {
    int input_flags, output_flags;
    if (!validate_channels(&input_flags, &output_flags) ||
        fcntl(RELEASE_FD, F_SETFL, input_flags | O_NONBLOCK) != 0 ||
        fcntl(STDERR_FILENO, F_SETFL, output_flags | O_NONBLOCK) != 0) {
        return false;
    }
    struct sigaction action, previous;
    memset(&action, 0, sizeof(action));
    action.sa_handler = SIG_IGN;
    if (sigemptyset(&action.sa_mask) != 0 || sigaction(SIGPIPE, &action, &previous) != 0) {
        return false;
    }
    bool released = exchange();
    /* Closing FD3 precedes the existing exact FD0/1/2 inventory and all collection. */
    bool closed = close(RELEASE_FD) == 0;
    bool restored = sigaction(SIGPIPE, &previous, NULL) == 0;
    return released && closed && restored;
}
