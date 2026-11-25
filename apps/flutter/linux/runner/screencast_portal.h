#pragma once

#include <flutter_linux/flutter_linux.h>
#include <gio/gio.h>

void register_screencast_portal(FlView* view, GDBusConnection* bus);
