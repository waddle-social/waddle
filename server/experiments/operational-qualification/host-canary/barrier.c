#define _GNU_SOURCE
#include "barrier.h"

#include <errno.h>
#include <poll.h>
#include <string.h>

static bool anonymous_pipe(const struct barrier_channel *channel, enum fd_access access) {
    if (!channel->fifo || classify_fd_access(channel->flags) != access || channel->inode == 0 ||
        channel->link_length < 8 || channel->link_length > 27 ||
        memcmp(channel->link, "pipe:[", 6) != 0 || channel->link[channel->link_length - 1] != ']' ||
        channel->link[6] == '0') {
        return false;
    }
    uint64_t inode = 0;
    for (size_t i = 6; i + 1 < channel->link_length; ++i) {
        unsigned char byte = (unsigned char)channel->link[i];
        if (byte < '0' || byte > '9' || inode > (UINT64_MAX - (byte - '0')) / 10u) {
            return false;
        }
        inode = inode * 10u + (byte - '0');
    }
    return inode == channel->inode;
}

bool barrier_channels_valid(const struct barrier_channel *release,
                            const struct barrier_channel *ready) {
    return anonymous_pipe(release, FD_READ_ONLY) && anonymous_pipe(ready, FD_WRITE_ONLY) &&
           (release->device != ready->device || release->inode != ready->inode);
}

static bool reject(struct barrier_state *state) {
    state->phase = BARRIER_REJECTED;
    return false;
}

static bool active(const struct barrier_state *state) {
    return state->phase == BARRIER_READY_WRITE || state->phase == BARRIER_RELEASE_READ;
}

bool barrier_start(struct barrier_state *state, bool clock_ok, uint64_t now_ms) {
    *state = (struct barrier_state){.phase = BARRIER_READY_WRITE, .last_ms = now_ms};
    if (!clock_ok || now_ms > UINT64_MAX - BARRIER_DEADLINE_MS) {
        return reject(state);
    }
    state->deadline_ms = now_ms + BARRIER_DEADLINE_MS;
    return true;
}

int barrier_time_left(struct barrier_state *state, bool clock_ok, uint64_t now_ms) {
    if (!active(state) || !clock_ok || now_ms < state->last_ms || now_ms >= state->deadline_ms) {
        reject(state);
        return 0;
    }
    state->last_ms = now_ms;
    return (int)(state->deadline_ms - now_ms);
}

int barrier_begin_attempt(struct barrier_state *state, bool clock_ok, uint64_t now_ms) {
    if (state->attempts >= BARRIER_MAX_ATTEMPTS) {
        reject(state);
        return 0;
    }
    ++state->attempts;
    return barrier_time_left(state, clock_ok, now_ms);
}

bool barrier_poll_event(struct barrier_state *state, int result, short events, int error,
                        bool *perform_io) {
    *perform_io = false;
    if (!active(state)) {
        return reject(state);
    }
    if (result < 0 && error == EINTR) {
        return true;
    }
    short allowed = state->phase == BARRIER_READY_WRITE ? POLLOUT : POLLIN | POLLHUP;
    if (result != 1 || events == 0 || (events & ~allowed) != 0) {
        return reject(state);
    }
    *perform_io = true;
    return true;
}

static bool retryable(int error) {
    return error == EINTR || error == EAGAIN || error == EWOULDBLOCK;
}

bool barrier_write_event(struct barrier_state *state, ssize_t count, int error) {
    if (state->phase != BARRIER_READY_WRITE) {
        return reject(state);
    }
    if (count < 0 && retryable(error)) {
        return true;
    }
    if (count <= 0 || (size_t)count > BARRIER_READY_BYTES - state->sent) {
        return reject(state);
    }
    state->sent += (size_t)count;
    if (state->sent == BARRIER_READY_BYTES) {
        state->phase = BARRIER_RELEASE_READ;
    }
    return true;
}

bool barrier_read_event(struct barrier_state *state, const char *bytes, ssize_t count, int error) {
    if (state->phase != BARRIER_RELEASE_READ) {
        return reject(state);
    }
    if (count < 0 && retryable(error)) {
        return true;
    }
    if (count == 0 && state->received) {
        state->phase = BARRIER_ACCEPTED;
        return true;
    }
    if (count != 1 || state->received || bytes[0] != 'I') {
        return reject(state);
    }
    state->received = true;
    return true;
}
