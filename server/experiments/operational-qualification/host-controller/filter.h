#ifndef WADDLE_CANARY_FILTER_H
#define WADDLE_CANARY_FILTER_H
#include <linux/filter.h>
#include <stddef.h>

#define FILTER_CAPACITY 256
/* Pure construction only. No syscall or installation occurs in filter.c. */
size_t canary_filter(struct sock_filter output[FILTER_CAPACITY], int witness);
#endif
