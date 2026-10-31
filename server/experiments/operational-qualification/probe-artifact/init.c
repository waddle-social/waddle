/* Guest-only PID 1. Never execute this binary during a host build or test. */
#define _GNU_SOURCE
#include "model.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/if.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <linux/sockios.h>
#include <poll.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

static struct inventory before, after;
static char output[REPORT_LIMIT];

static _Noreturn void halt_guest(void) {
    (void)alarm(0);
    (void)reboot(RB_POWER_OFF);
    (void)reboot(RB_HALT_SYSTEM);
    /* PID 1 exit is a kernel fail-stop if both reboot operations fail. */
    _Exit(111);
}

static _Noreturn void fatal(void) {
    static const char report[] = "{\"schema_version\":1,\"guest_checks\":\"inconclusive\","
        "\"host_isolation\":\"unverified\",\"qualification\":\"not_assessed\","
        "\"error\":\"guest_setup_or_output_failed\"}\n";
    if (write(STDOUT_FILENO, report, sizeof(report) - 1) != (ssize_t)(sizeof(report) - 1))
        halt_guest();
    halt_guest();
}

static void watchdog(int signal_number) {
    (void)signal_number;
    fatal();
}

static bool collect_dump(enum dump_kind kind, struct dump *dump) {
    int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
    if (fd < 0) return false;
    bool ok = false;
    struct timeval timeout = { .tv_sec = 0, .tv_usec = 250000 };
    struct sockaddr_nl local = { .nl_family = AF_NETLINK };
    if (setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout)) != 0 ||
        bind(fd, (struct sockaddr *)&local, sizeof(local)) != 0) goto finished;
    unsigned char request[REQUEST_LIMIT];
    size_t request_length = encode_request(kind, request);
    if (request_length == 0) goto finished;
    struct sockaddr_nl kernel = { .nl_family = AF_NETLINK };
    if (sendto(fd, request, request_length, MSG_DONTWAIT,
               (struct sockaddr *)&kernel, sizeof(kernel)) != (ssize_t)request_length)
        goto finished;
    for (unsigned attempt = 0; attempt < 8; ++attempt) {
        unsigned char packet[DUMP_LIMIT];
        struct sockaddr_nl peer = { 0 };
        struct iovec vector = { .iov_base = packet, .iov_len = sizeof(packet) };
        struct msghdr message = { .msg_name = &peer, .msg_namelen = sizeof(peer),
                                 .msg_iov = &vector, .msg_iovlen = 1 };
        ssize_t count = recvmsg(fd, &message, 0);
        if (count <= 0 || message.msg_flags & (MSG_TRUNC | MSG_CTRUNC) ||
            message.msg_namelen != sizeof(peer) || peer.nl_family != AF_NETLINK ||
            peer.nl_pid != 0 || peer.nl_groups != 0 ||
            (size_t)count > DUMP_LIMIT - dump->length) goto finished;
        memcpy(dump->bytes + dump->length, packet, (size_t)count);
        dump->length += (size_t)count;
        size_t cursor = 0;
        while (cursor < (size_t)count) {
            struct nlmsghdr header;
            if ((size_t)count - cursor < sizeof(header)) goto finished;
            memcpy(&header, packet + cursor, sizeof(header));
            if (header.nlmsg_len < sizeof(header) ||
                header.nlmsg_len > (size_t)count - cursor ||
                NLMSG_ALIGN(header.nlmsg_len) > (size_t)count - cursor)
                goto finished;
            if (header.nlmsg_type == NLMSG_DONE) {
                dump->complete = true;
                ok = inspect_dump(kind, dump) == EXPECTED;
                goto finished;
            }
            cursor += NLMSG_ALIGN(header.nlmsg_len);
        }
    }
finished:
    (void)close(fd);
    return ok;
}

static bool collect_inventory(struct inventory *inventory) {
    /* Complete all three dumps even when an earlier one is rejected. */
    bool links = collect_dump(LINKS, &inventory->dumps[LINKS]);
    bool addresses = collect_dump(ADDRESSES, &inventory->dumps[ADDRESSES]);
    bool routes = collect_dump(ROUTES, &inventory->dumps[ROUTES]);
    return links && addresses && routes;
}

static bool enable_loopback(void) {
    int fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, IPPROTO_UDP);
    if (fd < 0) return false;
    struct ifreq request = { .ifr_name = "lo" };
    bool ok = ioctl(fd, SIOCGIFFLAGS, &request) == 0;
    if (ok) {
        request.ifr_flags |= IFF_UP;
        ok = ioctl(fd, SIOCSIFFLAGS, &request) == 0;
    }
    (void)close(fd);
    return ok;
}

static socklen_t fixed_address(int family, bool loopback,
                              struct sockaddr_storage *storage) {
    memset(storage, 0, sizeof(*storage));
    if (family == AF_INET) {
        struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(39211) };
        /* RFC 5737 TEST-NET-1; never configurable or resolved with DNS. */
        static const unsigned char local[4] = { 127, 0, 0, 1 };
        static const unsigned char documentation[4] = { 192, 0, 2, 1 };
        memcpy(&address.sin_addr, loopback ? local : documentation, 4);
        memcpy(storage, &address, sizeof(address));
        return sizeof(address);
    }
    struct sockaddr_in6 address = { .sin6_family = AF_INET6, .sin6_port = htons(39211) };
    /* RFC 3849 documentation prefix. */
    static const unsigned char local[16] = { [15] = 1 };
    static const unsigned char documentation[16] = { 0x20, 0x01, 0x0d, 0xb8, [15] = 1 };
    memcpy(&address.sin6_addr, loopback ? local : documentation, 16);
    memcpy(storage, &address, sizeof(address));
    return sizeof(address);
}

static enum result udp_loopback(int family) {
    int receiver = socket(family, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, IPPROTO_UDP);
    if (receiver < 0) return classify_loopback(0, 0);
    int sender = socket(family, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, IPPROTO_UDP);
    enum result result = REJECTED;
    if (sender < 0) goto finished;
    struct sockaddr_storage address;
    socklen_t length = fixed_address(family, true, &address);
    static const unsigned char payload[] = { 0x57, 0x41, 0x44, 0x44, 0x4c, 0x45, 1 };
    if (bind(receiver, (struct sockaddr *)&address, length) != 0 ||
        connect(sender, (struct sockaddr *)&address, length) != 0 ||
        send(sender, payload, sizeof(payload), MSG_DONTWAIT) != (ssize_t)sizeof(payload))
        goto finished;
    struct pollfd ready = { .fd = receiver, .events = POLLIN };
    if (poll(&ready, 1, 250) != 1 || ready.revents != POLLIN) goto finished;
    unsigned char reply[sizeof(payload) + 1];
    ssize_t count = recv(receiver, reply, sizeof(reply), MSG_DONTWAIT | MSG_TRUNC);
    result = classify_loopback(1, count == (ssize_t)sizeof(payload) &&
                                  memcmp(reply, payload, sizeof(payload)) == 0);
finished:
    if (sender >= 0) (void)close(sender);
    (void)close(receiver);
    return result;
}

static enum result udp_no_route(int family) {
    int fd = socket(family, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, IPPROTO_UDP);
    if (fd < 0) return INCONCLUSIVE;
    struct sockaddr_storage address;
    socklen_t length = fixed_address(family, false, &address);
    /* UDP connect performs a route lookup; it transmits no application packet. */
    int outcome = connect(fd, (struct sockaddr *)&address, length);
    int error_number = errno;
    enum result result = classify_no_route(outcome, error_number);
    (void)close(fd);
    return result;
}

int main(void) {
    /* This guard precedes every mount, descriptor change and socket operation. */
    if (getpid() != 1) return 111;
    struct sigaction action = { .sa_handler = watchdog };
    if (sigemptyset(&action.sa_mask) != 0 || sigaction(SIGALRM, &action, NULL) != 0)
        halt_guest();
    (void)alarm(20);
    /* No proc/sysfs is needed: inventory uses only kernel rtnetlink messages. */
    if (mount("devtmpfs", "/dev", "devtmpfs", MS_NOSUID | MS_NOEXEC, "mode=0755") != 0)
        halt_guest();
    int console = open("/dev/console", O_WRONLY | O_NOCTTY | O_NONBLOCK | O_CLOEXEC);
    if (console < 0) halt_guest();
    if (dup2(console, STDOUT_FILENO) < 0 || dup2(console, STDERR_FILENO) < 0)
        halt_guest();
    if (console > STDERR_FILENO) (void)close(console);
    (void)close(STDIN_FILENO);
    struct controls controls = { INCONCLUSIVE, INCONCLUSIVE, INCONCLUSIVE, INCONCLUSIVE };
    if (collect_inventory(&before) && enable_loopback() && collect_inventory(&after) &&
        inventory_ready(&after)) {
        controls.loopback4 = udp_loopback(AF_INET);
        controls.loopback6 = udp_loopback(AF_INET6);
        /* A missing/failed positive control cannot establish a negative result. */
        if (controls.loopback4 == EXPECTED) controls.no_route4 = udp_no_route(AF_INET);
        if (controls.loopback6 == EXPECTED) controls.no_route6 = udp_no_route(AF_INET6);
    }
    if (!encode_report(output, sizeof(output), &before, &after, &controls)) fatal();
    size_t length = strlen(output);
    /* A partial or blocked output is failure; never retry indefinitely. */
    if (write(STDOUT_FILENO, output, length) != (ssize_t)length) fatal();
    halt_guest();
}
