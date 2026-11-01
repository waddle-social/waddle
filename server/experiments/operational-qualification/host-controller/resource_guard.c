/* Fixed benign inspection barrier. This is never a workload entrypoint. */
#define _GNU_SOURCE
#include "resource_paths.h"
#include <signal.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

extern char **environ;

int main(int argc, char **argv) {
  (void)argv;
  if (argc != 1 || getuid() == 0 || getuid() != geteuid() ||
      getgid() != getegid())
    return 64;
  (void)umask(S_IRWXG | S_IRWXO); //NOSONAR: deny group/other file permissions.
  struct rlimit zero = {0, 0};
  if (chdir("/") != 0 || syscall(SYS_close_range, 3U, ~0U, 0U) != 0 ||
      prctl(PR_SET_NO_NEW_PRIVS, 1L, 0L, 0L, 0L) != 0 ||
      setrlimit(RLIMIT_CORE, &zero) != 0 ||
      setrlimit(RLIMIT_FSIZE, &zero) != 0)
    return 74;
#ifdef RESOURCE_PARENT
  /* Prevent Python's locale coercion from introducing environment entries. */
  char *fixed_env[] = {"LC_ALL=C", NULL};
  char *args[] = {RESOURCE_PYTHON, "-I", "-S", RESOURCE_SCRIPT, NULL};
  execve(RESOURCE_PYTHON, args, fixed_env);
  return 74;
#else
  /* Empty the actual exec environment too, not merely libc's environment. */
  if (environ[0] != NULL) {
    char *empty[] = {NULL};
    char *args[] = {RESOURCE_GUARD, NULL};
    execve(RESOURCE_GUARD, args, empty);
    return 74;
  }
  /* No reads, writes, children or other work follow the observation barrier. */
  if (raise(SIGSTOP) != 0)
    return 74;
  _exit(0);
#endif
}
