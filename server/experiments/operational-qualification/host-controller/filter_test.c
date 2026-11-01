/* Pure cBPF interpreter fixtures. No filter installation or OS canary calls. */
#define _GNU_SOURCE
#include "filter.h"
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/seccomp.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>

static unsigned checks;
static int check(int condition) {
  ++checks;
  if (!condition) {
    fprintf(stderr, "filter assertion %u failed\n", checks);
    return -1;
  }
  return 0;
}

static uint32_t evaluate(const struct sock_filter *code, size_t count,
                         const struct seccomp_data *input) {
  uint32_t accumulator = 0;
  for (size_t pc = 0; pc < count; ++pc) {
    struct sock_filter op = code[pc];
    switch (op.code) {
    case BPF_LD | BPF_W | BPF_ABS:
      if (op.k > sizeof(*input) - 4)
        return 0;
      memcpy(&accumulator, (const unsigned char *)input + op.k, 4);
      break;
    case BPF_JMP | BPF_JEQ | BPF_K:
      pc += accumulator == op.k ? op.jt : op.jf;
      break;
    case BPF_JMP | BPF_JSET | BPF_K:
      pc += (accumulator & op.k) != 0 ? op.jt : op.jf;
      break;
    case BPF_RET | BPF_K:
      return op.k;
    default:
      return 0;
    }
  }
  return 0;
}

int main(void) {
  struct sock_filter witness[FILTER_CAPACITY], probe[FILTER_CAPACITY];
  size_t wc = canary_filter(witness, 1), pc = canary_filter(probe, 0);
  if (check(wc > 0 && pc > 0) != 0)
    return 1;
  struct seccomp_data input = {.arch = AUDIT_ARCH_X86_64, .nr = SYS_read};
  for (int nr = 0; nr < 600; ++nr) {
    input.nr = nr | 0x40000000;
    if (check(evaluate(witness, wc, &input) == SECCOMP_RET_KILL_PROCESS) != 0 ||
        check(evaluate(probe, pc, &input) == SECCOMP_RET_KILL_PROCESS) != 0)
      return 1;
  }
  input.nr = SYS_read;
  uint32_t arches[] = {0, AUDIT_ARCH_I386, AUDIT_ARCH_AARCH64, AUDIT_ARCH_ARM};
  for (size_t i = 0; i < sizeof(arches) / sizeof(arches[0]); ++i) {
    input.arch = arches[i];
    if (check(evaluate(witness, wc, &input) == SECCOMP_RET_KILL_PROCESS) != 0)
      return 1;
  }
  input.arch = AUDIT_ARCH_X86_64;
  const int denied[] = {SYS_keyctl,
                        SYS_add_key,
                        SYS_request_key,
                        SYS_io_uring_setup,
                        SYS_io_uring_enter,
                        SYS_io_uring_register,
                        SYS_ioctl,
                        SYS_ptrace,
                        SYS_bpf,
                        SYS_perf_event_open,
                        SYS_clone,
                        SYS_clone3,
                        SYS_fork,
                        SYS_vfork,
                        SYS_unshare,
                        SYS_setns,
                        SYS_mount,
                        SYS_mount_setattr,
                        SYS_umount2,
                        SYS_execveat,
                        SYS_memfd_create,
                        SYS_open_by_handle_at,
                        SYS_pidfd_getfd,
                        SYS_process_vm_readv,
                        SYS_process_vm_writev,
                        SYS_recvmsg,
                        SYS_sendmsg,
                        SYS_socketpair,
                        SYS_shmget,
                        SYS_shmat,
                        SYS_semget,
                        SYS_mq_open,
                        SYS_kill,
                        SYS_tgkill,
                        SYS_setgroups};
  for (size_t i = 0; i < sizeof(denied) / sizeof(denied[0]); ++i) {
    input.nr = denied[i];
    if (check(evaluate(witness, wc, &input) == (SECCOMP_RET_ERRNO | 1U)) != 0 ||
        check(evaluate(probe, pc, &input) == (SECCOMP_RET_ERRNO | 1U)) != 0)
      return 1;
  }
  input.nr = SYS_socket;
  input.args[1] = SOCK_STREAM | SOCK_CLOEXEC;
  for (unsigned domain = 0; domain < 128; ++domain) {
    input.args[0] = domain;
    uint32_t expected =
        domain == AF_UNIX ? SECCOMP_RET_ALLOW : SECCOMP_RET_ERRNO | 1U;
    if (check(evaluate(witness, wc, &input) == expected) != 0 ||
        check(evaluate(probe, pc, &input) == (SECCOMP_RET_ERRNO | 1U)) != 0)
      return 1;
  }
  input.args[0] = AF_UNIX;
  input.args[1] = SOCK_DGRAM | SOCK_CLOEXEC;
  if (check(evaluate(witness, wc, &input) == (SECCOMP_RET_ERRNO | 1U)) != 0)
    return 1;
  input.args[1] = SOCK_STREAM | SOCK_CLOEXEC;
  input.args[2] = 1;
  if (check(evaluate(witness, wc, &input) == (SECCOMP_RET_ERRNO | 1U)) != 0)
    return 1;
  memset(input.args, 0, sizeof(input.args));
  const int unix_ops[] = {SYS_connect, SYS_bind, SYS_listen, SYS_accept4};
  for (size_t i = 0; i < sizeof(unix_ops) / sizeof(unix_ops[0]); ++i) {
    input.nr = unix_ops[i];
    if (check(evaluate(witness, wc, &input) == SECCOMP_RET_ALLOW) != 0 ||
        check(evaluate(probe, pc, &input) == (SECCOMP_RET_ERRNO | 1U)) != 0)
      return 1;
  }
  input.nr = SYS_openat;
  const uint64_t bad_flags[] = {O_WRONLY, O_RDWR, O_CREAT,   O_TRUNC,
                                O_APPEND, O_PATH, O_TMPFILE, UINT64_C(1) << 32};
  for (size_t i = 0; i < sizeof(bad_flags) / sizeof(bad_flags[0]); ++i) {
    input.args[2] = bad_flags[i];
    if (check(evaluate(probe, pc, &input) == (SECCOMP_RET_ERRNO | 1U)) != 0)
      return 1;
  }
  input.args[2] = O_RDONLY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW;
  if (check(evaluate(probe, pc, &input) == SECCOMP_RET_ALLOW) != 0)
    return 1;
  for (int call = 0; call < 2; ++call) {
    input.nr = call == 0 ? SYS_mmap : SYS_mprotect;
    for (unsigned prot = 0; prot < 8; ++prot) {
      input.args[2] = prot;
      uint32_t expected =
          (prot & PROT_EXEC) != 0 ? SECCOMP_RET_ERRNO | 1U : SECCOMP_RET_ALLOW;
      if (check(evaluate(probe, pc, &input) == expected) != 0)
        return 1;
    }
  }
  input.nr = SYS_execve;
  if (check(evaluate(probe, pc, &input) == SECCOMP_RET_ALLOW) != 0)
    return 1;
  /* Explicitly retained for fixed /probe entry, never advertised as denied. */
  printf("%u pure filter assertions; witness=%zu probe=%zu instructions\n",
         checks, wc, pc);
  return 0;
}
