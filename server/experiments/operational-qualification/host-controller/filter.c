#define _GNU_SOURCE
#include "filter.h"
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>

#if !defined(__x86_64__) || defined(__ILP32__)
#error This reviewed filter profile supports x86_64 LP64 only
#endif

struct builder {
  struct sock_filter *out;
  size_t count;
  int failed;
};

static void emit(struct builder *b, unsigned short code, unsigned char jt,
                 unsigned char jf, uint32_t value) {
  if (b->count >= FILTER_CAPACITY) {
    b->failed = 1;
    return;
  }
  b->out[b->count++] = (struct sock_filter){code, jt, jf, value};
}

static void ret(struct builder *b, uint32_t value) {
  emit(b, BPF_RET | BPF_K, 0, 0, value);
}

static void load(struct builder *b, uint32_t offset) {
  emit(b, BPF_LD | BPF_W | BPF_ABS, 0, 0, offset);
}

static void equal(struct builder *b, uint32_t value) {
  emit(b, BPF_JMP | BPF_JEQ | BPF_K, 1, 0, value);
  ret(b, SECCOMP_RET_ERRNO | 1U);
}

static void arg_equal(struct builder *b, unsigned arg, uint32_t value) {
  uint32_t offset = (uint32_t)offsetof(struct seccomp_data, args) + arg * 8U;
  load(b, offset + 4U);
  equal(b, 0);
  load(b, offset);
  equal(b, value);
}

static void arg_mask(struct builder *b, unsigned arg, uint32_t forbidden) {
  uint32_t offset = (uint32_t)offsetof(struct seccomp_data, args) + arg * 8U;
  load(b, offset + 4U);
  equal(b, 0);
  load(b, offset);
  emit(b, BPF_JMP | BPF_JSET | BPF_K, 0, 1, forbidden);
  ret(b, SECCOMP_RET_ERRNO | 1U);
}

static size_t begin(struct builder *b, uint32_t nr) {
  load(b, (uint32_t)offsetof(struct seccomp_data, nr));
  size_t jump = b->count;
  emit(b, BPF_JMP | BPF_JEQ | BPF_K, 0, 0, nr);
  return jump;
}

static void end(struct builder *b, size_t jump) {
  ret(b, SECCOMP_RET_ALLOW);
  size_t skip = b->count - jump - 1;
  if (skip > 255 || jump >= FILTER_CAPACITY) {
    b->failed = 1;
    return;
  }
  b->out[jump].jf = (unsigned char)skip;
}

size_t canary_filter(struct sock_filter output[FILTER_CAPACITY], int witness) {
  struct builder b = {output, 0, 0};
  load(&b, (uint32_t)offsetof(struct seccomp_data, arch));
  emit(&b, BPF_JMP | BPF_JEQ | BPF_K, 1, 0, AUDIT_ARCH_X86_64);
  ret(&b, SECCOMP_RET_KILL_PROCESS);
  load(&b, (uint32_t)offsetof(struct seccomp_data, nr));
  emit(&b, BPF_JMP | BPF_JSET | BPF_K, 0, 1, 0x40000000U);
  ret(&b, SECCOMP_RET_KILL_PROCESS);
  static const uint32_t common[] = {SYS_read,
                                    SYS_close,
                                    SYS_fstat,
                                    SYS_newfstatat,
                                    SYS_stat,
                                    SYS_lstat,
                                    SYS_fstatfs,
                                    SYS_statfs,
                                    SYS_readlink,
                                    SYS_readlinkat,
                                    SYS_getdents64,
                                    SYS_poll,
                                    SYS_ppoll,
                                    SYS_clock_gettime,
                                    SYS_rt_sigaction,
                                    SYS_rt_sigprocmask,
                                    SYS_rt_sigreturn,
                                    SYS_getrlimit,
                                    SYS_exit,
                                    SYS_exit_group,
                                    SYS_brk,
                                    SYS_munmap,
                                    SYS_set_tid_address,
                                    SYS_set_robust_list,
                                    SYS_getpid,
                                    SYS_gettid,
                                    SYS_getuid,
                                    SYS_getgid,
                                    SYS_geteuid,
                                    SYS_getegid,
                                    SYS_execve};
  for (size_t i = 0; i < sizeof(common) / sizeof(common[0]); ++i) {
    size_t jump = begin(&b, common[i]);
    end(&b, jump);
  }
  size_t jump = begin(&b, SYS_write);
  if (!witness) {
    load(&b, (uint32_t)offsetof(struct seccomp_data, args));
    emit(&b, BPF_JMP | BPF_JEQ | BPF_K, 2, 0, 1);
    emit(&b, BPF_JMP | BPF_JEQ | BPF_K, 1, 0, 2);
    ret(&b, SECCOMP_RET_ERRNO | 1U);
  }
  end(&b, jump);
  uint32_t read_flags =
      O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK | O_DIRECTORY | O_LARGEFILE;
  jump = begin(&b, SYS_open);
  arg_mask(&b, 1, ~read_flags);
  end(&b, jump);
  jump = begin(&b, SYS_openat);
  arg_mask(&b, 2, ~read_flags);
  end(&b, jump);
  jump = begin(&b, SYS_mmap);
  arg_mask(&b, 2, ~(uint32_t)(PROT_READ | PROT_WRITE));
  end(&b, jump);
  jump = begin(&b, SYS_mprotect);
  arg_mask(&b, 2, ~(uint32_t)(PROT_READ | PROT_WRITE));
  end(&b, jump);
  jump = begin(&b, SYS_arch_prctl);
  arg_equal(&b, 0, 0x1002U); /* ARCH_SET_FS for static musl exec startup. */
  end(&b, jump);
  jump = begin(&b, SYS_prlimit64);
  arg_equal(&b, 0, 0);
  arg_equal(&b, 2, 0); /* Read limits only. */
  end(&b, jump);
  jump = begin(&b, SYS_fcntl);
  load(&b, (uint32_t)offsetof(struct seccomp_data, args) + 8U);
  emit(&b, BPF_JMP | BPF_JEQ | BPF_K, 2, 0, F_GETFL);
  emit(&b, BPF_JMP | BPF_JEQ | BPF_K, 1, 0, F_SETFL);
  ret(&b, SECCOMP_RET_ERRNO | 1U);
  end(&b, jump);
  if (witness) {
    jump = begin(&b, SYS_socket);
    arg_equal(&b, 0, AF_UNIX);
    arg_equal(&b, 1, SOCK_STREAM | SOCK_CLOEXEC);
    arg_equal(&b, 2, 0);
    end(&b, jump);
    static const uint32_t unix_calls[] = {SYS_connect, SYS_bind, SYS_listen,
                                          SYS_accept4};
    for (size_t i = 0; i < sizeof(unix_calls) / sizeof(unix_calls[0]); ++i) {
      jump = begin(&b, unix_calls[i]);
      end(&b, jump);
    }
    jump = begin(&b, SYS_prctl);
    arg_equal(&b, 0, PR_SET_SECCOMP);
    arg_equal(&b, 1, SECCOMP_MODE_FILTER);
    end(&b, jump);
  }
  ret(&b, SECCOMP_RET_ERRNO | 1U);
  return b.failed ? 0 : b.count;
}
