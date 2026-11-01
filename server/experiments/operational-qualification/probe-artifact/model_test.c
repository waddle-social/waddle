/* Host-runnable fixtures link only model.c, never init.c or its entry point. */
#include "model.h"

#include <assert.h>
#include <errno.h>
#include <linux/if.h>
#include <linux/if_link.h>
#include <linux/if_addr.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>

static struct inventory before, after;
static char report[REPORT_LIMIT];

static void add(struct dump *dump, unsigned type, const void *payload, size_t length) {
    struct nlmsghdr header = {
        .nlmsg_len = (unsigned)(sizeof(header) + length),
        .nlmsg_type = (unsigned short)type,
        .nlmsg_flags = NLM_F_MULTI,
        .nlmsg_seq = PROBE_SEQUENCE,
    };
    assert(dump->length + NLMSG_ALIGN(header.nlmsg_len) <= DUMP_LIMIT);
    memcpy(dump->bytes + dump->length, &header, sizeof(header));
    memcpy(dump->bytes + dump->length + sizeof(header), payload, length);
    dump->length += NLMSG_ALIGN(header.nlmsg_len);
}

static struct inventory fixture(void) {
    struct inventory inventory = { 0 };
    struct {
        struct ifinfomsg link;
        struct rtattr attribute;
        char name[4];
    } link = {
        .link = { .ifi_index = 1, .ifi_flags = IFF_LOOPBACK | IFF_UP },
        .attribute = { .rta_len = sizeof(struct rtattr) + 3, .rta_type = IFLA_IFNAME },
        .name = "lo",
    };
    add(&inventory.dumps[LINKS], RTM_NEWLINK, &link, sizeof(link));
    const unsigned families[] = { AF_INET, AF_INET6 };
    for (unsigned i = 0; i < 2; ++i) {
        unsigned char address[sizeof(struct ifaddrmsg) + sizeof(struct rtattr) + 16] = { 0 };
        struct ifaddrmsg address_header = { .ifa_family = (unsigned char)families[i],
            .ifa_index = 1, .ifa_prefixlen = i == 0 ? 8 : 128 };
        unsigned bytes = i == 0 ? 4 : 16;
        struct rtattr address_attribute = { .rta_type = IFA_ADDRESS,
            .rta_len = (unsigned short)(sizeof(struct rtattr) + bytes) };
        memcpy(address, &address_header, sizeof(address_header));
        memcpy(address + sizeof(address_header), &address_attribute, sizeof(address_attribute));
        unsigned char *value = address + sizeof(address_header) + sizeof(address_attribute);
        if (i == 0) value[0] = 127;
        value[bytes - 1] = 1;
        add(&inventory.dumps[ADDRESSES], RTM_NEWADDR, address,
            sizeof(address_header) + sizeof(address_attribute) + bytes);

        unsigned char route[sizeof(struct rtmsg) + 2 * sizeof(struct rtattr) + 16 + 4] = { 0 };
        struct rtmsg route_header = { .rtm_family = (unsigned char)families[i],
            .rtm_table = RT_TABLE_LOCAL, .rtm_type = RTN_LOCAL,
            .rtm_dst_len = i == 0 ? 32 : 128 };
        struct rtattr destination = { .rta_type = RTA_DST,
            .rta_len = (unsigned short)(sizeof(struct rtattr) + bytes) };
        struct rtattr output = { .rta_type = RTA_OIF, .rta_len = sizeof(struct rtattr) + 4 };
        uint32_t index = 1;
        size_t cursor = 0;
        memcpy(route, &route_header, sizeof(route_header)); cursor += sizeof(route_header);
        memcpy(route + cursor, &destination, sizeof(destination)); cursor += sizeof(destination);
        memcpy(route + cursor, value, bytes); cursor += bytes;
        memcpy(route + cursor, &output, sizeof(output)); cursor += sizeof(output);
        memcpy(route + cursor, &index, sizeof(index)); cursor += sizeof(index);
        add(&inventory.dumps[ROUTES], RTM_NEWROUTE, route, cursor);
    }
    for (unsigned i = 0; i < 3; ++i) {
        int status = 0;
        add(&inventory.dumps[i], NLMSG_DONE, &status, sizeof(status));
        inventory.dumps[i].complete = true;
    }
    return inventory;
}

int main(void) {
    const size_t bodies[] = { sizeof(struct ifinfomsg), sizeof(struct ifaddrmsg), sizeof(struct rtmsg) };
    const unsigned types[] = { RTM_GETLINK, RTM_GETADDR, RTM_GETROUTE };
    for (unsigned i = 0; i < 3; ++i) {
        unsigned char request[REQUEST_LIMIT];
        size_t length = encode_request((enum dump_kind)i, request);
        struct nlmsghdr header;
        memcpy(&header, request, sizeof(header));
        assert(length == sizeof(header) + bodies[i]);
        assert(header.nlmsg_len == length && header.nlmsg_type == types[i]);
        assert(header.nlmsg_flags == (NLM_F_REQUEST | NLM_F_DUMP));
        assert(header.nlmsg_seq == PROBE_SEQUENCE && header.nlmsg_pid == 0);
        for (size_t j = sizeof(header); j < length; ++j) assert(request[j] == 0);
    }
    before = fixture();
    after = fixture();
    assert(inventory_ready(&after));
    struct controls controls = { EXPECTED, EXPECTED, EXPECTED, EXPECTED };
    assert(classify_report(&before, &after, &controls) == EXPECTED);
    assert(encode_report(report, sizeof(report), &before, &after, &controls));
    assert(strstr(report, "\"guest_checks\":\"expected\"") != NULL);
    assert(strstr(report, "\"host_isolation\":\"unverified\"") != NULL);
    size_t exact_length = strlen(report);
    char exact[REPORT_LIMIT];
    assert(encode_report(exact, exact_length + 1, &before, &after, &controls));
    assert(strcmp(exact, report) == 0);
    assert(!encode_report(exact, exact_length, &before, &after, &controls));
    assert(exact[0] == '\0');

    assert(classify_loopback(0, 0) == INCONCLUSIVE);
    assert(classify_loopback(1, 0) == REJECTED);
    controls.loopback6 = INCONCLUSIVE;
    assert(classify_report(&before, &after, &controls) == INCONCLUSIVE);
    assert(encode_report(report, sizeof(report), &before, &after, &controls));
    assert(strstr(report, "\"guest_checks\":\"inconclusive\"") != NULL);
    controls.loopback6 = EXPECTED;

    /* A complete dump without IPv6 still cannot qualify the positive inventory. */
    struct dump missing = { 0 };
    struct nlmsghdr first;
    memcpy(&first, after.dumps[ADDRESSES].bytes, sizeof(first));
    size_t first_record = first.nlmsg_len;
    memcpy(missing.bytes, after.dumps[ADDRESSES].bytes, first_record);
    missing.length = first_record;
    int done = 0;
    add(&missing, NLMSG_DONE, &done, sizeof(done));
    missing.complete = true;
    after.dumps[ADDRESSES] = missing;
    assert(inspect_dump(ADDRESSES, &missing) == EXPECTED);
    assert(!inventory_ready(&after));
    assert(classify_report(&before, &after, &controls) == INCONCLUSIVE);
    after = fixture();

    assert(classify_no_route(-1, ENETUNREACH) == EXPECTED);
    const int wrong[] = { EPERM, EACCES, ETIMEDOUT, EHOSTUNREACH, EINVAL, EAGAIN, 0 };
    for (unsigned i = 0; i < sizeof(wrong) / sizeof(wrong[0]); ++i)
        assert(classify_no_route(-1, wrong[i]) == REJECTED);
    assert(classify_no_route(0, ENETUNREACH) == REJECTED);
    assert(classify_no_route(-1, EAFNOSUPPORT) == INCONCLUSIVE);
    assert(classify_no_route(-1, EPROTONOSUPPORT) == INCONCLUSIVE);

    after.dumps[ADDRESSES].complete = false;
    assert(classify_report(&before, &after, &controls) == INCONCLUSIVE);
    after = fixture();
    --after.dumps[ROUTES].length;
    assert(inspect_dump(ROUTES, &after.dumps[ROUTES]) == INCONCLUSIVE);
    after = fixture();
    struct ifinfomsg unknown = { .ifi_index = 2, .ifi_flags = IFF_LOOPBACK };
    memcpy(after.dumps[LINKS].bytes + sizeof(struct nlmsghdr), &unknown, sizeof(unknown));
    assert(inspect_dump(LINKS, &after.dumps[LINKS]) == REJECTED);
    assert(classify_report(&before, &after, &controls) == REJECTED);
    after = fixture();
    struct nlmsghdr interrupted;
    memcpy(&interrupted, after.dumps[ROUTES].bytes, sizeof(interrupted));
    interrupted.nlmsg_flags |= NLM_F_DUMP_INTR;
    memcpy(after.dumps[ROUTES].bytes, &interrupted, sizeof(interrupted));
    assert(inspect_dump(ROUTES, &after.dumps[ROUTES]) == INCONCLUSIVE);

    after = fixture();
    assert(!encode_report(report, 20, &before, &after, &controls));
    assert(report[0] == '\0');
    assert(!encode_report(report, 0, &before, &after, &controls));
    after.dumps[LINKS].length = DUMP_LIMIT + 1;
    assert(!encode_report(report, sizeof(report), &before, &after, &controls));
    assert(report[0] == '\0');
    puts("model fixtures passed (guest entry point never linked)");
    return 0;
}
