#include "model.h"

#include <errno.h>
#include <linux/if.h>
#include <linux/if_addr.h>
#include <linux/if_link.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>

size_t encode_request(enum dump_kind kind, unsigned char output[REQUEST_LIMIT]) {
    size_t body;
    unsigned type;
    switch (kind) {
    case LINKS: body = sizeof(struct ifinfomsg); type = RTM_GETLINK; break;
    case ADDRESSES: body = sizeof(struct ifaddrmsg); type = RTM_GETADDR; break;
    case ROUTES: body = sizeof(struct rtmsg); type = RTM_GETROUTE; break;
    default: return 0;
    }
    struct nlmsghdr header = {
        .nlmsg_len = (unsigned)(sizeof(header) + body),
        .nlmsg_type = (unsigned short)type,
        .nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP,
        .nlmsg_seq = PROBE_SEQUENCE,
    };
    /* The fixed request families and all reserved fields are zero (AF_UNSPEC). */
    memset(output, 0, REQUEST_LIMIT);
    memcpy(output, &header, sizeof(header));
    return header.nlmsg_len;
}

static bool address_is_loopback(unsigned family, const unsigned char *bytes,
                                size_t length) {
    static const unsigned char loopback6[16] = { [15] = 1 };
    return (family == AF_INET && length == 4 && bytes[0] == 127) ||
           (family == AF_INET6 && length == 16 &&
            memcmp(bytes, loopback6, 16) == 0);
}

static bool inspect_record(enum dump_kind kind, const unsigned char *data,
                           size_t length) {
    size_t fixed;
    unsigned family = 0;
    bool found = false;
    if (kind == LINKS) {
        struct ifinfomsg link;
        if (length < sizeof(link)) return false;
        memcpy(&link, data, sizeof(link));
        if (link.ifi_index != 1 || !(link.ifi_flags & IFF_LOOPBACK)) return false;
        fixed = sizeof(link);
    } else if (kind == ADDRESSES) {
        struct ifaddrmsg address;
        if (length < sizeof(address)) return false;
        memcpy(&address, data, sizeof(address));
        family = address.ifa_family;
        if (address.ifa_index != 1 ||
            !((family == AF_INET && address.ifa_prefixlen == 8) ||
              (family == AF_INET6 && address.ifa_prefixlen == 128))) return false;
        fixed = sizeof(address);
    } else {
        struct rtmsg route;
        if (length < sizeof(route)) return false;
        memcpy(&route, data, sizeof(route));
        family = route.rtm_family;
        if ((family != AF_INET && family != AF_INET6) ||
            (route.rtm_table != RT_TABLE_LOCAL && route.rtm_table != RT_TABLE_MAIN) ||
            (route.rtm_type != RTN_LOCAL && route.rtm_type != RTN_UNICAST &&
             route.rtm_type != RTN_BROADCAST) || route.rtm_src_len != 0 ||
            (family == AF_INET && (route.rtm_dst_len < 8 || route.rtm_dst_len > 32)) ||
            (family == AF_INET6 && route.rtm_dst_len != 128)) return false;
        fixed = sizeof(route);
    }
    size_t cursor = NLMSG_ALIGN(fixed);
    bool has_output = false;
    while (cursor < length) {
        struct rtattr attribute;
        if (length - cursor < sizeof(attribute)) return false;
        memcpy(&attribute, data + cursor, sizeof(attribute));
        size_t size = attribute.rta_len;
        if (size < sizeof(attribute) || size > length - cursor ||
            RTA_ALIGN(size) > length - cursor) return false;
        const unsigned char *value = data + cursor + sizeof(attribute);
        size_t count = size - sizeof(attribute);
        unsigned type = attribute.rta_type;
        if (kind == LINKS && type == IFLA_IFNAME) {
            if (found || count != 3 || memcmp(value, "lo\0", 3) != 0) return false;
            found = true;
        } else if (kind == ADDRESSES && (type == IFA_ADDRESS || type == IFA_LOCAL)) {
            if (!address_is_loopback(family, value, count)) return false;
            found = true;
        } else if (kind == ROUTES && type == RTA_DST) {
            if (found || !address_is_loopback(family, value, count)) return false;
            found = true;
        } else if (kind == ROUTES && type == RTA_OIF) {
            uint32_t index;
            if (has_output || count != sizeof(index)) return false;
            memcpy(&index, value, sizeof(index));
            if (index != 1) return false;
            has_output = true;
        } else if (kind == ROUTES && type != RTA_PRIORITY && type != RTA_PREFSRC &&
                   type != RTA_CACHEINFO && type != RTA_PREF && type != RTA_TABLE) {
            /* In particular: no gateway, multipath, encapsulation or nexthop ID. */
            return false;
        }
        if (kind == ROUTES && type == RTA_TABLE) {
            uint32_t table;
            if (count != sizeof(table)) return false;
            memcpy(&table, value, sizeof(table));
            if (table != RT_TABLE_LOCAL && table != RT_TABLE_MAIN) return false;
        }
        cursor += RTA_ALIGN(size);
    }
    return cursor == length && found && (kind != ROUTES || has_output);
}

enum result inspect_dump(enum dump_kind kind, const struct dump *dump) {
    if (!dump->complete || dump->length > DUMP_LIMIT) return INCONCLUSIVE;
    size_t cursor = 0, records = 0;
    bool done = false;
    while (cursor < dump->length) {
        struct nlmsghdr header;
        if (done || dump->length - cursor < sizeof(header)) return INCONCLUSIVE;
        memcpy(&header, dump->bytes + cursor, sizeof(header));
        size_t size = header.nlmsg_len;
        if (size < sizeof(header) || size > dump->length - cursor ||
            NLMSG_ALIGN(size) > dump->length - cursor ||
            header.nlmsg_seq != PROBE_SEQUENCE || (header.nlmsg_flags & NLM_F_DUMP_INTR))
            return INCONCLUSIVE;
        const unsigned char *payload = dump->bytes + cursor + sizeof(header);
        size_t count = size - sizeof(header);
        if (header.nlmsg_type == NLMSG_DONE) {
            int status;
            if (count != sizeof(status)) return INCONCLUSIVE;
            memcpy(&status, payload, sizeof(status));
            if (status != 0) return INCONCLUSIVE;
            done = true;
        } else {
            unsigned expected = kind == LINKS ? RTM_NEWLINK :
                                kind == ADDRESSES ? RTM_NEWADDR : RTM_NEWROUTE;
            if (header.nlmsg_type != expected ||
                !(header.nlmsg_flags & NLM_F_MULTI)) return INCONCLUSIVE;
            if (!inspect_record(kind, payload, count)) return REJECTED;
            ++records;
        }
        cursor += NLMSG_ALIGN(size);
    }
    if (!done || (kind == LINKS && records != 1)) return INCONCLUSIVE;
    return EXPECTED;
}

enum result classify_no_route(int operation_result, int error_number) {
    if (operation_result == -1 && error_number == ENETUNREACH) return EXPECTED;
    if (operation_result == -1 && (error_number == EAFNOSUPPORT ||
                                  error_number == EPROTONOSUPPORT)) return INCONCLUSIVE;
    return REJECTED;
}

enum result classify_loopback(int family_available, int exact_reply) {
    if (!family_available) return INCONCLUSIVE;
    return exact_reply ? EXPECTED : REJECTED;
}

bool inventory_ready(const struct inventory *inventory) {
    for (unsigned i = 0; i < 3; ++i)
        if (inspect_dump((enum dump_kind)i, &inventory->dumps[i]) != EXPECTED) return false;
    struct ifinfomsg link;
    memcpy(&link, inventory->dumps[LINKS].bytes + sizeof(struct nlmsghdr), sizeof(link));
    if (!(link.ifi_flags & IFF_UP)) return false;
    for (unsigned i = ADDRESSES; i <= ROUTES; ++i) {
        const struct dump *dump = &inventory->dumps[i];
        unsigned families = 0;
        size_t cursor = 0;
        while (cursor < dump->length) {
            struct nlmsghdr header;
            memcpy(&header, dump->bytes + cursor, sizeof(header));
            if (header.nlmsg_type != NLMSG_DONE) {
                unsigned family = dump->bytes[cursor + sizeof(header)];
                if (family == AF_INET) families |= 1;
                if (family == AF_INET6) families |= 2;
            }
            cursor += NLMSG_ALIGN(header.nlmsg_len);
        }
        if (families != 3) return false;
    }
    return true;
}

enum result classify_report(const struct inventory *before,
                            const struct inventory *after,
                            const struct controls *controls) {
    enum result result = EXPECTED;
    for (unsigned i = 0; i < 3; ++i) {
        enum result a = inspect_dump((enum dump_kind)i, &before->dumps[i]);
        enum result b = inspect_dump((enum dump_kind)i, &after->dumps[i]);
        if (a == REJECTED || b == REJECTED) return REJECTED;
        if (a == INCONCLUSIVE || b == INCONCLUSIVE) result = INCONCLUSIVE;
    }
    if (!inventory_ready(after)) result = INCONCLUSIVE;
    const enum result checks[] = { controls->loopback4, controls->loopback6,
                                  controls->no_route4, controls->no_route6 };
    for (unsigned i = 0; i < 4; ++i) {
        if (checks[i] == REJECTED) return REJECTED;
        if (checks[i] != EXPECTED) result = INCONCLUSIVE;
    }
    return result;
}

static const char *result_name(enum result result) {
    switch (result) {
    case EXPECTED: return "expected";
    case REJECTED: return "rejected";
    default: return "inconclusive";
    }
}

struct writer { char *bytes; size_t capacity, length; bool valid; };

static void append(struct writer *writer, const char *format, ...) {
    if (!writer->valid) return;
    va_list arguments;
    va_start(arguments, format);
    int count = vsnprintf(writer->bytes + writer->length,
                         writer->capacity - writer->length, format, arguments);
    va_end(arguments);
    if (count < 0 || (size_t)count >= writer->capacity - writer->length) {
        writer->valid = false;
        return;
    }
    writer->length += (size_t)count;
}

static void encode_inventory(struct writer *writer, const struct inventory *inventory) {
    static const char *const names[] = { "interfaces", "addresses", "routes" };
    static const char hex[] = "0123456789abcdef";
    append(writer, "{");
    for (unsigned i = 0; i < 3; ++i) {
        const struct dump *dump = &inventory->dumps[i];
        append(writer, "%s\"%s\":{\"status\":\"%s\",\"rtnetlink_hex\":\"",
               i == 0 ? "" : ",", names[i],
               result_name(inspect_dump((enum dump_kind)i, dump)));
        if (dump->length > DUMP_LIMIT) { writer->valid = false; return; }
        for (size_t j = 0; j < dump->length; ++j)
            append(writer, "%c%c", hex[dump->bytes[j] >> 4], hex[dump->bytes[j] & 15]);
        append(writer, "\"}");
    }
    append(writer, "}");
}

bool encode_report(char *output, size_t capacity, const struct inventory *before,
                   const struct inventory *after, const struct controls *controls) {
    if (capacity == 0 || capacity > REPORT_LIMIT) return false;
    struct writer writer = { output, capacity, 0, true };
    append(&writer, "{\"schema_version\":1,\"guest_checks\":\"%s\","
           "\"host_isolation\":\"unverified\",\"qualification\":\"not_assessed\","
           "\"inventory_format\":\"linux-rtnetlink-native-little-endian-v1\",\"before\":",
           result_name(classify_report(before, after, controls)));
    encode_inventory(&writer, before);
    append(&writer, ",\"after\":");
    encode_inventory(&writer, after);
    append(&writer, ",\"udp_loopback_v4\":\"%s\",\"udp_loopback_v6\":\"%s\","
           "\"udp_no_route_v4\":\"%s\",\"udp_no_route_v6\":\"%s\"}\n",
           result_name(controls->loopback4), result_name(controls->loopback6),
           result_name(controls->no_route4), result_name(controls->no_route6));
    if (!writer.valid) output[0] = '\0';
    return writer.valid;
}
