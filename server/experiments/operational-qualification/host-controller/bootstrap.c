#define _GNU_SOURCE
#include "paths.h"
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

_Static_assert(
    (S_IRWXG | S_IRWXO) ==
            (S_IRGRP | S_IWGRP | S_IXGRP | S_IROTH | S_IWOTH | S_IXOTH) &&
        ((S_IRWXG | S_IRWXO) & S_IRWXU) == 0,
    "Mask every group/other permission and preserve owner permissions");

static int nonce(const char *s) {
  return strlen(s) == 32 && strspn(s, "0123456789abcdef") == 32;
}

int main(int argc, char **argv) {
  char invocation[33] = {0};
  const char *provided = getenv("INVOCATION_ID");
  if (provided != NULL && nonce(provided))
    memcpy(invocation, provided, 32);
  if (clearenv() != 0 || getuid() == 0 || getuid() != geteuid() ||
      getgid() != getegid())
    return 64;
  /* Deny every group/other permission on subsequently created
   * files/directories. */
  (void)umask(S_IRWXG | S_IRWXO); //NOSONAR: deny group/other file permissions.
  if (chdir("/") != 0)
    return 74;
  if (!((argc == 2 && strcmp(argv[1], "run") == 0) ||
        (argc == 3 && nonce(argv[2]) &&
         (strcmp(argv[1], "service") == 0 || strcmp(argv[1], "cleanup") == 0))))
    return 64;
  if (strcmp(argv[1], "service") == 0 && invocation[0] == 0)
    return 64;
  if (syscall(SYS_close_range, 3U, ~0U, 0U) != 0 ||
      prctl(PR_SET_NO_NEW_PRIVS, 1L, 0L, 0L, 0L) != 0)
    return 74;
  struct rlimit core = {0, 0};
  if (setrlimit(RLIMIT_CORE, &core) != 0)
    return 74;
  if (setenv("WADDLE_STATIC_BOOTSTRAP", "1", 1) != 0 ||
      setenv("WADDLE_INVOCATION_ID", invocation, 1) != 0)
    return 74;
  char *args[] = {PYTHON_PATH, "-I",
                  "-S",        CONTROLLER_PATH,
                  argv[1],     argc == 3 ? argv[2] : NULL,
                  NULL};
  extern char **environ;
  execve(PYTHON_PATH, args, environ);
  return 74;
}
