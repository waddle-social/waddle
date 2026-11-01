#define _GNU_SOURCE
#include "probe.h"
#ifdef PROBE_CONTROLLED
#include "barrier.h"
#endif

#include <stdlib.h>

int main(int argc, char **argv) {
    (void)argv;
    /* No switches, commands, paths, input streams or inherited configuration. */
    if (argc != 1 || clearenv() != 0) {
        return 64;
    }
#ifdef PROBE_CONTROLLED
    if (!await_controller_release()) {
        return 74;
    }
#endif
    struct probe_report report;
    inspect_self(&report);
    char frame[FRAME_BYTES];
    size_t length;
    if (report.fd_state != OBSERVED || !format_frame(&report, frame, sizeof(frame), &length) ||
        !write_frame(frame, length)) {
        return 74;
    }
    /* This component can never return a successful host-canary result. */
    return 2;
}
