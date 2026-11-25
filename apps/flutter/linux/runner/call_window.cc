#include "call_window.h"

namespace {
constexpr gint kCompactWidth = 420;
constexpr gint kCompactHeight = 300;

struct CallWindow {
  GtkWindow* window;
  bool active = false;
  bool compact = false;
  bool was_maximized = false;
  bool was_fullscreen = false;
  gint normal_width = 1100;
  gint normal_height = 760;
};

gboolean configure(GtkWidget* widget, GdkEventConfigure*, gpointer data) {
  auto* state = static_cast<CallWindow*>(data);
  const auto flags = gdk_window_get_state(gtk_widget_get_window(widget));
  if (!state->compact &&
      !(flags & (GDK_WINDOW_STATE_MAXIMIZED | GDK_WINDOW_STATE_FULLSCREEN))) {
    gtk_window_get_size(state->window, &state->normal_width,
                        &state->normal_height);
  }
  return FALSE;
}

gboolean window_state(GtkWidget*, GdkEventWindowState* event, gpointer data) {
  auto* state = static_cast<CallWindow*>(data);
  const auto large = GDK_WINDOW_STATE_MAXIMIZED | GDK_WINDOW_STATE_FULLSCREEN;
  if (state->compact && (event->changed_mask & large) &&
      !(event->new_window_state & large)) {
    gtk_window_resize(state->window, kCompactWidth, kCompactHeight);
  }
  return FALSE;
}

void set_compact(FlMethodChannel* channel, CallWindow* state, bool compact) {
  if (state->compact == compact) return;
  auto* window = state->window;
  state->compact = compact;
  if (compact) {
    auto flags =
        gdk_window_get_state(gtk_widget_get_window(GTK_WIDGET(window)));
    state->was_fullscreen = flags & GDK_WINDOW_STATE_FULLSCREEN;
    state->was_maximized = flags & GDK_WINDOW_STATE_MAXIMIZED;
    if (!state->was_fullscreen && !state->was_maximized) {
      gtk_window_get_size(window, &state->normal_width, &state->normal_height);
    }
    if (state->was_fullscreen) gtk_window_unfullscreen(window);
    if (state->was_maximized) gtk_window_unmaximize(window);
    gtk_window_resize(window, kCompactWidth, kCompactHeight);
  } else {
    gtk_window_resize(window, state->normal_width, state->normal_height);
    if (state->was_maximized) gtk_window_maximize(window);
    if (state->was_fullscreen) gtk_window_fullscreen(window);
  }
  g_autoptr(FlValue) result = fl_value_new_bool(compact);
  fl_method_channel_invoke_method(channel, "changed", result, nullptr, nullptr,
                                  nullptr);
}

void handle(FlMethodChannel* channel, FlMethodCall* call, gpointer data) {
  auto* state = static_cast<CallWindow*>(data);
  const char* method = fl_method_call_get_name(call);
  if (!state->window) {
    fl_method_call_respond_error(
        call, "closed-window", "The call window has closed", nullptr, nullptr);
    return;
  }
  if (g_str_equal(method, "setCompact") || g_str_equal(method, "setActive")) {
    auto* value = fl_method_call_get_args(call);
    if (fl_value_get_type(value) != FL_VALUE_TYPE_BOOL) {
      fl_method_call_respond_error(call, "invalid-state", "Expected a boolean",
                                   nullptr, nullptr);
      return;
    }
    const bool enabled = fl_value_get_bool(value);
    if (g_str_equal(method, "setActive")) {
      state->active = enabled;
      if (!enabled) set_compact(channel, state, false);
      fl_method_call_respond_success(call, nullptr, nullptr);
      return;
    }
    set_compact(channel, state, state->active && enabled);
  } else if (!g_str_equal(method, "state")) {
    fl_method_call_respond_not_implemented(call, nullptr);
    return;
  }
  g_autoptr(FlValue) result = fl_value_new_bool(state->compact);
  fl_method_call_respond_success(call, result, nullptr);
}
}  // namespace

void register_call_window(FlView* view, GtkWindow* window) {
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  g_autoptr(FlMethodChannel) channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)),
      "social.waddle.flutter/call_window", FL_METHOD_CODEC(codec));
  auto* state = new CallWindow{window};
  g_object_add_weak_pointer(G_OBJECT(window),
                            reinterpret_cast<gpointer*>(&state->window));
  g_signal_connect(window, "configure-event", G_CALLBACK(configure), state);
  g_signal_connect(window, "window-state-event", G_CALLBACK(window_state),
                   state);
  fl_method_channel_set_method_call_handler(
      channel, handle, state, [](gpointer data) {
        auto* state = static_cast<CallWindow*>(data);
        if (state->window) {
          g_signal_handlers_disconnect_by_data(state->window, state);
          g_object_remove_weak_pointer(
              G_OBJECT(state->window),
              reinterpret_cast<gpointer*>(&state->window));
        }
        delete state;
      });
}
