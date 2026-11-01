#define _GNU_SOURCE
#include "barrier.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned assertions;

#define CHECK(expression)                                                                          \
    do {                                                                                           \
        ++assertions;                                                                              \
        if (!(expression)) {                                                                       \
            fprintf(stderr, "barrier fixture assertion failed at line %d\n", __LINE__);            \
            exit(1);                                                                               \
        }                                                                                          \
    } while (0)

static struct barrier_state ready_state(void) {
    struct barrier_state state;
    CHECK(barrier_start(&state, true, 100));
    CHECK(state.phase == BARRIER_READY_WRITE && state.sent == 0 && !state.received);
    return state;
}

static struct barrier_state release_state(void) {
    struct barrier_state state = ready_state();
    CHECK(barrier_write_event(&state, (ssize_t)BARRIER_READY_BYTES, 0));
    CHECK(state.phase == BARRIER_RELEASE_READ);
    return state;
}

static void channel_tests(void) {
    struct barrier_channel input = {.fifo = true,
                                    .flags = O_RDONLY,
                                    .device = 1,
                                    .inode = 100,
                                    .link = "pipe:[100]",
                                    .link_length = 10};
    struct barrier_channel output = {.fifo = true,
                                     .flags = O_WRONLY,
                                     .device = 1,
                                     .inode = 101,
                                     .link = "pipe:[101]",
                                     .link_length = 10};
    CHECK(barrier_channels_valid(&input, &output));
    const int flags[] = {O_RDONLY,
                         O_WRONLY,
                         O_RDWR,
                         O_ACCMODE,
                         -1,
                         O_PATH,
                         O_PATH | O_WRONLY,
                         O_PATH | O_NONBLOCK | O_NOFOLLOW,
                         O_RDONLY | O_NONBLOCK,
                         O_WRONLY | O_NONBLOCK};
    for (size_t i = 0; i < sizeof(flags) / sizeof(flags[0]); ++i) {
        input.flags = flags[i];
        CHECK(barrier_channels_valid(&input, &output) ==
              (flags[i] == O_RDONLY || flags[i] == (O_RDONLY | O_NONBLOCK)));
        input.flags = O_RDONLY;
        output.flags = flags[i];
        CHECK(barrier_channels_valid(&input, &output) ==
              (flags[i] == O_WRONLY || flags[i] == (O_WRONLY | O_NONBLOCK)));
        output.flags = O_WRONLY;
    }
    const char *bad_links[] = {"",
                               "pipe:[0]",
                               "pipe:[0100]",
                               "pipe:[-1]",
                               "pipe:[101]",
                               "pipe:[100]x",
                               "pipe:[100]\n",
                               "pipe:[100",
                               "socket:[100]",
                               "/run/synthetic-fifo",
                               "pipe:[18446744073709551616]"};
    for (size_t i = 0; i < sizeof(bad_links) / sizeof(bad_links[0]); ++i) {
        input.link = bad_links[i];
        input.link_length = strlen(input.link);
        CHECK(!barrier_channels_valid(&input, &output));
    }
    input.link = "pipe:[100]";
    input.link_length = 10;
    for (size_t i = 0; i < 2; ++i) {
        struct barrier_channel *value = i == 0 ? &input : &output;
        value->fifo = false;
        CHECK(!barrier_channels_valid(&input, &output));
        value->fifo = true;
        uint64_t inode = value->inode;
        value->inode = 0;
        CHECK(!barrier_channels_valid(&input, &output));
        value->inode = inode;
    }
    output.inode = 100;
    output.link = "pipe:[100]";
    CHECK(!barrier_channels_valid(&input, &output));
    output.device = 2;
    CHECK(barrier_channels_valid(&input, &output));
    input.inode = UINT64_MAX;
    input.link = "pipe:[18446744073709551615]";
    input.link_length = strlen(input.link);
    CHECK(barrier_channels_valid(&input, &output));
    input.link = "pipe:[10\0]";
    input.link_length = 10;
    CHECK(!barrier_channels_valid(&input, &output));
}

static void ready_tests(void) {
    CHECK(BARRIER_READY_BYTES == 12);
    CHECK(memcmp(BARRIER_READY, "PROBE_READY\n", BARRIER_READY_BYTES) == 0);
    for (size_t split = 1; split < BARRIER_READY_BYTES; ++split) {
        struct barrier_state state = ready_state();
        CHECK(barrier_write_event(&state, (ssize_t)split, 0));
        CHECK(state.phase == BARRIER_READY_WRITE && state.sent == split);
        CHECK(barrier_write_event(&state, -1, EINTR));
        CHECK(barrier_write_event(&state, -1, EAGAIN));
        CHECK(state.sent == split);
        CHECK(barrier_write_event(&state, (ssize_t)(BARRIER_READY_BYTES - split), 0));
        CHECK(state.phase == BARRIER_RELEASE_READ && state.sent == BARRIER_READY_BYTES);
        CHECK(!barrier_write_event(&state, 1, 0));
    }
    const ssize_t invalid[] = {0, -1, (ssize_t)BARRIER_READY_BYTES + 1};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        struct barrier_state state = ready_state();
        CHECK(!barrier_write_event(&state, invalid[i], EPIPE));
        CHECK(state.phase == BARRIER_REJECTED);
    }
    struct barrier_state state = ready_state();
    CHECK(!barrier_read_event(&state, "I", 1, 0));
}

static void release_tests(void) {
    struct barrier_state state = release_state();
    CHECK(barrier_read_event(&state, "I", 1, 0));
    CHECK(state.phase == BARRIER_RELEASE_READ && state.received);
    CHECK(barrier_read_event(&state, NULL, -1, EINTR));
    CHECK(barrier_read_event(&state, NULL, -1, EAGAIN));
    CHECK(barrier_read_event(&state, NULL, -1, EWOULDBLOCK));
    CHECK(barrier_read_event(&state, NULL, 0, 0));
    CHECK(state.phase == BARRIER_ACCEPTED);
    CHECK(!barrier_read_event(&state, "I", 1, 0));
    const char *invalid[] = {"X", "i", "\n", "II", "Ix", "I\n"};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        state = release_state();
        CHECK(!barrier_read_event(&state, invalid[i], (ssize_t)strlen(invalid[i]), 0));
        CHECK(state.phase == BARRIER_REJECTED);
    }
    state = release_state();
    CHECK(!barrier_read_event(&state, NULL, 0, 0));
    state = release_state();
    CHECK(!barrier_read_event(&state, NULL, -1, EIO));
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        state = release_state();
        CHECK(barrier_read_event(&state, "I", 1, 0));
        CHECK(!barrier_read_event(&state, invalid[i], (ssize_t)strlen(invalid[i]), 0));
    }
    state = release_state();
    CHECK(barrier_read_event(&state, "I", 1, 0));
    CHECK(barrier_time_left(&state, true, 100 + BARRIER_DEADLINE_MS) == 0);
    CHECK(state.phase == BARRIER_REJECTED); /* A byte without EOF never releases. */
}

static void poll_tests(void) {
    const short flags[] = {0,       POLLIN,   POLLOUT,          POLLHUP,           POLLIN | POLLHUP,
                           POLLERR, POLLNVAL, POLLIN | POLLERR, POLLOUT | POLLHUP, POLLPRI};
    for (size_t phase = 0; phase < 2; ++phase) {
        for (size_t i = 0; i < sizeof(flags) / sizeof(flags[0]); ++i) {
            struct barrier_state state = phase == 0 ? ready_state() : release_state();
            bool io = false;
            bool valid = phase == 0 ? flags[i] == POLLOUT
                                    : flags[i] == POLLIN || flags[i] == POLLHUP ||
                                          flags[i] == (POLLIN | POLLHUP);
            CHECK(barrier_poll_event(&state, 1, flags[i], 0, &io) == valid);
            CHECK(io == valid);
        }
    }
    struct barrier_state state = release_state();
    bool io;
    CHECK(barrier_poll_event(&state, 1, POLLIN | POLLHUP, 0, &io) && io);
    CHECK(barrier_read_event(&state, "I", 1, 0));
    CHECK(barrier_poll_event(&state, 1, POLLHUP, 0, &io) && io);
    CHECK(barrier_read_event(&state, NULL, 0, 0));
    CHECK(state.phase == BARRIER_ACCEPTED);
    const int results[] = {0, -1, 2};
    for (size_t i = 0; i < sizeof(results) / sizeof(results[0]); ++i) {
        state = release_state();
        CHECK(!barrier_poll_event(&state, results[i], POLLIN, EIO, &io));
        CHECK(!io && state.phase == BARRIER_REJECTED);
    }
    state = ready_state();
    CHECK(barrier_poll_event(&state, -1, 0, EINTR, &io) && !io);
}

static void deadline_tests(void) {
    struct barrier_state state;
    CHECK(!barrier_start(&state, false, 0));
    CHECK(!barrier_start(&state, true, UINT64_MAX - BARRIER_DEADLINE_MS + 1));
    CHECK(barrier_start(&state, true, UINT64_MAX - BARRIER_DEADLINE_MS));
    CHECK(barrier_time_left(&state, true, UINT64_MAX - 1) == 1);
    CHECK(barrier_time_left(&state, true, UINT64_MAX) == 0);
    state = ready_state();
    CHECK(barrier_time_left(&state, true, 100) == BARRIER_DEADLINE_MS);
    CHECK(barrier_time_left(&state, true, 101) == BARRIER_DEADLINE_MS - 1);
    CHECK(barrier_time_left(&state, true, 100) == 0);
    state = ready_state();
    CHECK(barrier_time_left(&state, false, 101) == 0);
    state = release_state();
    CHECK(barrier_time_left(&state, false, 101) == 0);
    for (size_t kind = 0; kind < 5; ++kind) {
        state = kind == 0 || kind >= 3 ? ready_state() : release_state();
        for (unsigned i = 0; i < BARRIER_MAX_ATTEMPTS; ++i) {
            CHECK(barrier_begin_attempt(&state, true, 100) == BARRIER_DEADLINE_MS);
            if (kind == 0) {
                bool io;
                CHECK(barrier_poll_event(&state, -1, 0, EINTR, &io) && !io);
            } else if (kind < 3) {
                CHECK(barrier_read_event(&state, NULL, -1, kind == 1 ? EINTR : EAGAIN));
            } else {
                CHECK(barrier_write_event(&state, -1, kind == 3 ? EINTR : EAGAIN));
            }
        }
        CHECK(barrier_begin_attempt(&state, true, 100) == 0);
        CHECK(state.phase == BARRIER_REJECTED && state.attempts == BARRIER_MAX_ATTEMPTS);
    }
}

int main(void) {
    channel_tests();
    ready_tests();
    release_tests();
    poll_tests();
    deadline_tests();
    fprintf(stderr, "%u pure barrier fixture assertions passed\n", assertions);
    return 0;
}
