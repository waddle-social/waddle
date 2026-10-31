#ifndef WADDLE_HOST_PROBE_BARRIER_H
#define WADDLE_HOST_PROBE_BARRIER_H

#include "probe.h"

#include <sys/types.h>

#define BARRIER_READY "PROBE_READY\n"
#define BARRIER_READY_BYTES (sizeof(BARRIER_READY) - 1u)
#define BARRIER_DEADLINE_MS 5000u
#define BARRIER_MAX_ATTEMPTS 128u

enum barrier_phase {
    BARRIER_READY_WRITE,
    BARRIER_RELEASE_READ,
    BARRIER_ACCEPTED,
    BARRIER_REJECTED
};

struct barrier_state {
    enum barrier_phase phase;
    size_t sent;
    bool received;
    uint64_t last_ms;
    uint64_t deadline_ms;
    unsigned attempts;
};

struct barrier_channel {
    bool fifo;
    int flags;
    uint64_t device;
    uint64_t inode;
    const char *link;
    size_t link_length;
};

/* Pure descriptor classification and protocol transitions; no OS operations. */
bool barrier_channels_valid(const struct barrier_channel *release,
                            const struct barrier_channel *ready);
bool barrier_start(struct barrier_state *state, bool clock_ok, uint64_t now_ms);
int barrier_time_left(struct barrier_state *state, bool clock_ok, uint64_t now_ms);
int barrier_begin_attempt(struct barrier_state *state, bool clock_ok, uint64_t now_ms);
bool barrier_poll_event(struct barrier_state *state, int result, short events, int error,
                        bool *perform_io);
bool barrier_write_event(struct barrier_state *state, ssize_t count, int error);
bool barrier_read_event(struct barrier_state *state, const char *bytes, ssize_t count, int error);

/* OS adapter: never linked into either synthetic fixture binary. */
bool await_controller_release(void);

#endif
