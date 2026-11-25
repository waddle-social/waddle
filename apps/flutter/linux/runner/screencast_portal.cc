#include "screencast_portal.h"

#include <gtk/gtk.h>

#include <algorithm>
#include <memory>
#include <set>
#include <string>

namespace {
constexpr char kPortal[] = "org.freedesktop.portal.Desktop";
constexpr char kSession[] = "org.freedesktop.portal.Session";
constexpr char kScreenCast[] = "org.freedesktop.portal.ScreenCast";

// All state changes run on GTK's main thread. The bus filter sees only
// messages sent by this process and forwards session paths to that thread.
struct Sessions {
  GDBusConnection* bus;
  GWeakRef channel;
  std::set<std::string> paths;
  bool watching = false;
  bool enumerated = false;
  bool active = false;
  gint64 token = 0;
  FlMethodCall* waiting = nullptr;
  std::string start_request;

  explicit Sessions(GDBusConnection* connection)
      : bus(G_DBUS_CONNECTION(g_object_ref(connection))) {
    g_weak_ref_init(&channel, nullptr);
  }
  ~Sessions() {
    g_clear_object(&waiting);
    g_weak_ref_clear(&channel);
    g_object_unref(bus);
  }

  void close(const char* path) {
    g_dbus_connection_call(bus, kPortal, path, kSession, "Close", nullptr,
                           nullptr, G_DBUS_CALL_FLAGS_NONE, -1, nullptr,
                           nullptr, nullptr);
  }

  void finish_wait(bool ready) {
    if (!waiting) return;
    g_autoptr(FlValue) value = fl_value_new_bool(ready);
    fl_method_call_respond_success(waiting, value, nullptr);
    g_clear_object(&waiting);
  }

  void selected(const std::string& path) {
    const auto slash = path.rfind('/');
    if (!watching || slash == std::string::npos ||
        path.compare(slash + 1, sizeof("webrtc_session") - 1,
                     "webrtc_session") != 0) {
      return;
    }
    // libwebrtc m150 starts a separate ScreenCast session when enumerating
    // its synthetic Wayland screen for thumbnails. Close this session before
    // Start can present another picker. Its source ID remains available.
    if (!enumerated) {
      enumerated = true;
      close(path.c_str());
      finish_wait(active);
    } else if (active) {
      paths.insert(path);
    } else {
      close(path.c_str());
    }
  }

  void stop() {
    active = false;
    start_request.clear();
    finish_wait(false);
    for (const auto& path : paths) close(path.c_str());
    paths.clear();
  }
};

using Shared = std::shared_ptr<Sessions>;

struct Selection {
  Shared sessions;
  std::string path;
  std::string request;
};

GDBusMessage* filter(GDBusConnection*, GDBusMessage* message, gboolean incoming,
                     gpointer data) {
  if (!incoming &&
      g_dbus_message_get_message_type(message) ==
          G_DBUS_MESSAGE_TYPE_METHOD_CALL &&
      g_strcmp0(g_dbus_message_get_interface(message), kScreenCast) == 0) {
    GVariant* body = g_dbus_message_get_body(message);
    const auto* method = g_dbus_message_get_member(message);
    const bool select = g_strcmp0(method, "SelectSources") == 0 && body &&
                        g_variant_is_of_type(body, G_VARIANT_TYPE("(oa{sv})"));
    const bool start = g_strcmp0(method, "Start") == 0 && body &&
                       g_variant_is_of_type(body, G_VARIANT_TYPE("(osa{sv})"));
    if (select || start) {
      g_autoptr(GVariant) handle = g_variant_get_child_value(body, 0);
      const auto& sessions = *static_cast<Shared*>(data);
      std::string request;
      if (start) {
        g_autoptr(GVariant) options = g_variant_get_child_value(body, 2);
        const char* token = nullptr;
        if (!g_variant_lookup(options, "handle_token", "&s", &token))
          return message;
        std::string sender =
            g_dbus_connection_get_unique_name(sessions->bus) + 1;
        std::replace(sender.begin(), sender.end(), '.', '_');
        request =
            "/org/freedesktop/portal/desktop/request/" + sender + "/" + token;
      }
      auto* selection = new Selection{
          sessions, g_variant_get_string(handle, nullptr), request};
      g_main_context_invoke_full(
          nullptr, G_PRIORITY_HIGH,
          [](gpointer arg) -> gboolean {
            auto* event = static_cast<Selection*>(arg);
            if (event->request.empty()) {
              event->sessions->selected(event->path);
            } else if (event->sessions->paths.count(event->path)) {
              event->sessions->start_request = event->request;
            }
            return G_SOURCE_REMOVE;
          },
          selection, [](gpointer arg) { delete static_cast<Selection*>(arg); });
    }
  }
  return message;
}

void responded(GDBusConnection*, const gchar*, const gchar* path, const gchar*,
               const gchar*, GVariant* args, gpointer data) {
  auto& sessions = **static_cast<Shared*>(data);
  if (!sessions.active || sessions.start_request != path) return;
  sessions.start_request.clear();
  if (!g_variant_is_of_type(args, G_VARIANT_TYPE("(ua{sv})"))) return;
  g_autoptr(GVariant) code = g_variant_get_child_value(args, 0);
  g_autoptr(GVariant) results = g_variant_get_child_value(args, 1);
  g_autoptr(GVariant) streams =
      g_variant_lookup_value(results, "streams", G_VARIANT_TYPE("a(ua{sv})"));
  const bool allowed = g_variant_get_uint32(code) == 0 && streams &&
                       g_variant_n_children(streams) > 0;
  g_autoptr(FlMethodChannel) channel =
      FL_METHOD_CHANNEL(g_weak_ref_get(&sessions.channel));
  if (channel) {
    g_autoptr(FlValue) token = fl_value_new_int(sessions.token);
    fl_method_channel_invoke_method(channel, allowed ? "granted" : "revoked",
                                    token, nullptr, nullptr, nullptr);
  }
}

void closed(GDBusConnection*, const gchar*, const gchar* path, const gchar*,
            const gchar*, GVariant*, gpointer data) {
  auto& sessions = **static_cast<Shared*>(data);
  if (!sessions.paths.erase(path) || !sessions.active) return;
  g_autoptr(FlMethodChannel) channel =
      FL_METHOD_CHANNEL(g_weak_ref_get(&sessions.channel));
  if (channel) {
    g_autoptr(FlValue) token = fl_value_new_int(sessions.token);
    fl_method_channel_invoke_method(channel, "revoked", token, nullptr, nullptr,
                                    nullptr);
  }
}

struct Registration {
  Shared sessions;
  guint filter_id;
  guint subscription_id;
  guint response_id;
  ~Registration() {
    sessions->stop();
    g_dbus_connection_remove_filter(sessions->bus, filter_id);
    g_dbus_connection_signal_unsubscribe(sessions->bus, subscription_id);
    g_dbus_connection_signal_unsubscribe(sessions->bus, response_id);
  }
};

void handle(FlMethodChannel*, FlMethodCall* call, gpointer data) {
  auto& sessions = *static_cast<Registration*>(data)->sessions;
  const char* method = fl_method_call_get_name(call);
  if (g_str_equal(method, "prepare")) {
    auto* display = gdk_display_get_default();
    if (!display ||
        !g_str_equal(G_OBJECT_TYPE_NAME(display), "GdkWaylandDisplay")) {
      fl_method_call_respond_success(call, nullptr, nullptr);
      return;
    }
    FlValue* arg = fl_method_call_get_args(call);
    if (fl_value_get_type(arg) != FL_VALUE_TYPE_INT) {
      fl_method_call_respond_error(call, "invalid-capture",
                                   "Missing capture identifier", nullptr,
                                   nullptr);
      return;
    }
    sessions.stop();
    sessions.watching = true;
    sessions.active = true;
    sessions.token = fl_value_get_int(arg);
    g_autoptr(FlValue) value = fl_value_new_bool(!sessions.enumerated);
    fl_method_call_respond_success(call, value, nullptr);
  } else if (g_str_equal(method, "capture")) {
    if (sessions.active && !sessions.enumerated && !sessions.waiting) {
      sessions.waiting = FL_METHOD_CALL(g_object_ref(call));
      return;
    }
    g_autoptr(FlValue) value =
        fl_value_new_bool(sessions.active && sessions.enumerated);
    fl_method_call_respond_success(call, value, nullptr);
  } else if (g_str_equal(method, "stop")) {
    sessions.stop();
    fl_method_call_respond_success(call, nullptr, nullptr);
  } else {
    fl_method_call_respond_not_implemented(call, nullptr);
  }
}
}  // namespace

void register_screencast_portal(FlView* view, GDBusConnection* bus) {
  if (!bus) return;
  auto sessions = std::make_shared<Sessions>(bus);
  auto destroy = [](gpointer data) { delete static_cast<Shared*>(data); };
  guint filter_id =
      g_dbus_connection_add_filter(bus, filter, new Shared(sessions), destroy);
  guint subscription_id = g_dbus_connection_signal_subscribe(
      bus, kPortal, kSession, "Closed", nullptr, nullptr,
      G_DBUS_SIGNAL_FLAGS_NONE, closed, new Shared(sessions), destroy);
  guint response_id = g_dbus_connection_signal_subscribe(
      bus, kPortal, "org.freedesktop.portal.Request", "Response", nullptr,
      nullptr, G_DBUS_SIGNAL_FLAGS_NONE, responded, new Shared(sessions),
      destroy);
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  g_autoptr(FlMethodChannel) channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)),
      "social.waddle.flutter/screen_capture", FL_METHOD_CODEC(codec));
  g_weak_ref_set(&sessions->channel, channel);
  fl_method_channel_set_method_call_handler(
      channel, handle,
      new Registration{sessions, filter_id, subscription_id, response_id},
      [](gpointer data) { delete static_cast<Registration*>(data); });
}
