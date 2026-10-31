#ifndef WADDLE_PROBE_MODEL_H
#define WADDLE_PROBE_MODEL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define DUMP_LIMIT 8192U
#define REPORT_LIMIT (DUMP_LIMIT * 12U + 4096U)
#define PROBE_SEQUENCE 1U
#define REQUEST_LIMIT 64U

enum dump_kind { LINKS, ADDRESSES, ROUTES };
enum result { INCONCLUSIVE, REJECTED, EXPECTED };
struct dump {
    unsigned char bytes[DUMP_LIMIT];
    size_t length;
    bool complete;
};
struct inventory {
    struct dump dumps[3];
};
struct controls {
    enum result loopback4, loopback6, no_route4, no_route6;
};

/* Pure functions: no files, sockets, environment, clock or process operations. */
size_t encode_request(enum dump_kind kind, unsigned char output[REQUEST_LIMIT]);
enum result inspect_dump(enum dump_kind kind, const struct dump *dump);
bool inventory_ready(const struct inventory *inventory);
enum result classify_no_route(int operation_result, int error_number);
enum result classify_loopback(int family_available, int exact_reply);
enum result classify_report(const struct inventory *before,
                            const struct inventory *after,
                            const struct controls *controls);
bool encode_report(char *output, size_t capacity, const struct inventory *before,
                   const struct inventory *after, const struct controls *controls);

#endif
