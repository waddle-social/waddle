#include "my_application.h"

int main(int argc, char** argv) {
#ifdef WADDLE_EGL_VENDOR_DIRS
  g_setenv("__EGL_VENDOR_LIBRARY_DIRS", WADDLE_EGL_VENDOR_DIRS, FALSE);
#endif
  g_autoptr(MyApplication) app = my_application_new();
  return g_application_run(G_APPLICATION(app), argc, argv);
}
