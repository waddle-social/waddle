/* Trusted fixed setup and sentinel witness. NEVER executed by build/tests.
 * This setup profile requires the independent architecture/final review gate.
 */
#define _GNU_SOURCE
#include "filter.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/capability.h>
#include <linux/mount.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <unistd.h>

static int install_filter(int witness) {
  struct sock_filter instructions[FILTER_CAPACITY];
  size_t count = canary_filter(instructions, witness);
  if (count == 0 || count > 65535)
    return -1;
  struct sock_fprog program = {(unsigned short)count, instructions};
  return prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program);
}

static int exact_write(int fd, const char *data, size_t size) {
  return write(fd, data, size) == (ssize_t)size ? 0 : -1;
}

static int gate_byte(int fd, char expected) {
  struct pollfd wait = {.fd = fd, .events = POLLIN};
  if (poll(&wait, 1, 5000) != 1 || (wait.revents & (POLLERR | POLLNVAL)) != 0 ||
      (wait.revents & (POLLIN | POLLHUP)) == 0)
    return -1;
  char byte;
  return read(fd, &byte, 1) == 1 && byte == expected ? 0 : -1;
}

static int proc_write(int proc, const char *path, const char *data) {
  int fd = openat(proc, path, O_WRONLY | O_CLOEXEC | O_NOFOLLOW);
  if (fd < 0)
    return -1;
  int result = exact_write(fd, data, strlen(data));
  if (close(fd) != 0)
    result = -1;
  return result;
}

static int finish_boundary(void) {
  /* Simultaneous user+mount unshare gives the new user namespace ownership
   * of the new mount namespace. Add restrictive mount flags only AFTER maps
   * are written. No untrusted code runs with these setup capabilities. */
  int proc = open("/proc", O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
  if (proc < 0)
    return -1;
  if (proc_write(proc, "sys/user/max_user_namespaces", "1\n") != 0 ||
      unshare(CLONE_NEWUSER | CLONE_NEWNS) != 0 ||
      proc_write(proc, "self/uid_map", "0 0 1\n") != 0 ||
      proc_write(proc, "self/setgroups", "deny\n") != 0 ||
      proc_write(proc, "self/gid_map", "0 0 1\n") != 0) {
    (void)close(proc);
    return -1;
  }
  if (close(proc) != 0)
    return -1;
  /* Add only restrictions, preserving every inherited/locked mount flag. */
  struct mount_attr attrs = {.attr_set = MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID |
                                         MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC,
                             .attr_clr = 0,
                             .propagation = 0,
                             .userns_fd = 0};
  if (syscall(SYS_mount_setattr, AT_FDCWD, "/", 0U, &attrs, sizeof(attrs)) !=
          0 ||
      syscall(SYS_mount_setattr, AT_FDCWD, "/proc", 0U, &attrs,
              sizeof(attrs)) != 0)
    return -1;
  attrs.attr_set = MOUNT_ATTR_NOSUID | MOUNT_ATTR_NOEXEC;
  if (syscall(SYS_mount_setattr, AT_FDCWD, "/dev/null", 0U, &attrs,
              sizeof(attrs)) != 0)
    return -1;
  attrs.attr_set = MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV;
  if (syscall(SYS_mount_setattr, AT_FDCWD, "/probe", 0U, &attrs,
              sizeof(attrs)) != 0)
    return -1;
  /* Drop SETPCAP last, with no assumption about future cap_last_cap values. */
  int last = -1;
  for (int cap = 0; cap < 256; ++cap) {
    int result = prctl(PR_CAPBSET_READ, cap, 0L, 0L, 0L);
    if (result < 0) {
      if (errno != EINVAL || cap == 0)
        return -1;
      last = cap - 1;
      break;
    }
    if (cap != CAP_SETPCAP && prctl(PR_CAPBSET_DROP, cap, 0L, 0L, 0L) != 0)
      return -1;
  }
  if (last < 0 || prctl(PR_CAPBSET_DROP, CAP_SETPCAP, 0L, 0L, 0L) != 0)
    return -1;
  struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
  struct __user_cap_data_struct caps[2] = {{0, 0, 0}, {0, 0, 0}};
  if (syscall(SYS_capset, &header, caps) != 0 ||
      prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0L, 0L, 0L) != 0 ||
      prctl(PR_SET_NO_NEW_PRIVS, 1L, 0L, 0L, 0L) != 0 ||
      prctl(PR_SET_DUMPABLE, 1L, 0L, 0L, 0L) != 0)
    return -1;
  errno = 0;
  if (unshare(CLONE_NEWUSER) == 0 || (errno != EPERM && errno != ENOSPC))
    return -1;
  return 0;
}

static int address(struct sockaddr_un *out, const char *name, int abstract,
                   socklen_t *length) {
  size_t size = strlen(name);
  if (size == 0 || size + 1 >= sizeof(out->sun_path))
    return -1;
  memset(out, 0, sizeof(*out));
  out->sun_family = AF_UNIX;
  memcpy(out->sun_path + (abstract ? 1 : 0), name, size);
  *length = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + size + 1);
  return 0;
}

static int negative(const char *name, int abstract, int expected) {
  struct sockaddr_un addr;
  socklen_t size;
  if (address(&addr, name, abstract, &size) != 0)
    return -1;
  int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0)
    return -1;
  int result = connect(fd, (struct sockaddr *)&addr, size);
  int error = errno;
  (void)close(fd);
  return result < 0 && error == expected ? 0 : -1;
}

static int local_positive(const char *name, const char *nonce) {
  struct sockaddr_un addr;
  socklen_t size;
  int listener = -1, client = -1, accepted = -1, result = -1;
  char value[32];
  if (address(&addr, name, 1, &size) != 0)
    return -1;
  listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  client = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (listener < 0 || client < 0 ||
      bind(listener, (struct sockaddr *)&addr, size) != 0 ||
      listen(listener, 1) != 0 ||
      connect(client, (struct sockaddr *)&addr, size) != 0)
    goto done;
  accepted = accept4(listener, NULL, NULL, SOCK_CLOEXEC);
  if (accepted < 0 || exact_write(client, nonce, 32) != 0 ||
      read(accepted, value, sizeof(value)) != 32 ||
      memcmp(value, nonce, 32) != 0 || exact_write(accepted, nonce, 32) != 0 ||
      read(client, value, sizeof(value)) != 32 || memcmp(value, nonce, 32) != 0)
    goto done;
  result = 0;
done:
  if (accepted >= 0)
    (void)close(accepted);
  if (client >= 0)
    (void)close(client);
  if (listener >= 0)
    (void)close(listener);
  return result;
}

int main(int argc, char **argv) {
  if (clearenv() != 0 || argc != 4 || strlen(argv[1]) != 32 ||
      strspn(argv[1], "0123456789abcdef") != 32 || strlen(argv[2]) > 10 ||
      strspn(argv[2], "0123456789") != strlen(argv[2]) || strlen(argv[3]) > 2 ||
      strspn(argv[3], "0123456789") != strlen(argv[3]))
    return 64;
  unsigned long uid = strtoul(argv[2], NULL, 10);
  int gate = atoi(argv[3]);
  if (uid == 0 || uid >= 4294967295UL || gate < 3 || gate >= 64)
    return 64;
  /* Namespace PID 1 ignores default-action SIGALRM. The external observer
   * and service enforce deadlines; each local gate also has a bounded poll. */
  for (int fd = 3; fd < 64; ++fd)
    if (fd != gate)
      (void)close(fd);
  if (gate != 3) {
    if (syscall(SYS_dup3, gate, 3, 0) != 3 || close(gate) != 0)
      return 74;
    gate = 3;
  } else if (fcntl(gate, F_SETFD, 0) != 0) {
    return 74;
  }
  if (fcntl(gate, F_SETFL, O_NONBLOCK) != 0 ||
      fcntl(2, F_SETFL, O_WRONLY | O_NONBLOCK) != 0)
    return 74;
  if (finish_boundary() != 0 || install_filter(1) != 0 ||
      exact_write(2, "READY\n", 6) != 0)
    return 74;
  if (gate_byte(gate, 'G') != 0)
    return 74;
  char file[108], path[108], abstract[64], local[64];
  int count = snprintf(file, sizeof(file),
                       "/dev/shm/waddle-host-%lu-%s/host-file", uid, argv[1]);
  if (count <= 0 || (size_t)count >= sizeof(file))
    return 74;
  count = snprintf(path, sizeof(path), "/dev/shm/waddle-host-%lu-%s/host.sock",
                   uid, argv[1]);
  if (count <= 0 || (size_t)count >= sizeof(path))
    return 74;
  count = snprintf(abstract, sizeof(abstract), "waddle-host-%s", argv[1]);
  if (count <= 0 || (size_t)count >= sizeof(abstract))
    return 74;
  count = snprintf(local, sizeof(local), "waddle-local-%s", argv[1]);
  if (count <= 0 || (size_t)count >= sizeof(local))
    return 74;
  errno = 0;
  int fd = open(file, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
  int error = errno;
  if (fd >= 0) {
    (void)close(fd);
    return 74;
  }
  if (error != ENOENT || negative(path, 0, ENOENT) != 0 ||
      negative(abstract, 1, ECONNREFUSED) != 0 ||
      local_positive(local, argv[1]) != 0)
    return 74;
  static const char result[] =
      "SENTINELS file=ENOENT path=ENOENT abstract=ECONNREFUSED local=ok\n";
  if (exact_write(2, result, sizeof(result) - 1) != 0)
    return 74;
  if (install_filter(0) != 0 || exact_write(2, "INSPECTOR_READY\n", 16) != 0 ||
      gate_byte(gate, 'P') != 0)
    return 74;
  char *args[] = {"/probe", NULL};
  char *env[] = {NULL};
  execve("/probe", args, env);
  return 74;
}
