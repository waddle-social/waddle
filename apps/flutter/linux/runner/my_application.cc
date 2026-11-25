#include "my_application.h"

#include <flutter_linux/flutter_linux.h>

#include "flutter/generated_plugin_registrant.h"

struct _MyApplication {
  GtkApplication parent_instance;
  gchar** dart_arguments;
};

G_DEFINE_TYPE(MyApplication, my_application, GTK_TYPE_APPLICATION)

static void first_frame(FlView* view, gpointer) {
  gtk_widget_show(gtk_widget_get_toplevel(GTK_WIDGET(view)));
}

static void activate(GApplication* app) {
  GtkApplication* gtk_app = GTK_APPLICATION(app);
  GList* windows = gtk_application_get_windows(gtk_app);
  if (windows != nullptr) {
    gtk_window_present(GTK_WINDOW(windows->data));
    return;
  }

  GtkWindow* window = GTK_WINDOW(gtk_application_window_new(gtk_app));
  gtk_window_set_title(window, "Waddle");
  gtk_window_set_default_size(window, 1100, 760);

  g_autoptr(FlDartProject) project = fl_dart_project_new();
  fl_dart_project_set_dart_entrypoint_arguments(project, MY_APPLICATION(app)->dart_arguments);
  FlView* view = fl_view_new(project);
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(view));
  gtk_widget_show(GTK_WIDGET(view));
  g_signal_connect(view, "first-frame", G_CALLBACK(first_frame), nullptr);
  gtk_widget_realize(GTK_WIDGET(view));
  fl_register_plugins(FL_PLUGIN_REGISTRY(view));
  gtk_widget_grab_focus(GTK_WIDGET(view));
}

static gboolean local_command_line(GApplication* app, gchar*** arguments, int* status) {
  // Flutter forwards VM tooling arguments to the primary Dart isolate.
  MY_APPLICATION(app)->dart_arguments = g_strdupv(*arguments + 1);
  g_autoptr(GError) error = nullptr;
  if (!g_application_register(app, nullptr, &error)) {
    g_warning("Could not register Waddle: %s", error->message);
    *status = 1;
    return TRUE;
  }

  g_application_activate(app);
  *status = 0;
  return TRUE;
}

static void dispose(GObject* object) {
  g_clear_pointer(&MY_APPLICATION(object)->dart_arguments, g_strfreev);
  G_OBJECT_CLASS(my_application_parent_class)->dispose(object);
}

static void my_application_class_init(MyApplicationClass* klass) {
  G_APPLICATION_CLASS(klass)->activate = activate;
  G_APPLICATION_CLASS(klass)->local_command_line = local_command_line;
  G_OBJECT_CLASS(klass)->dispose = dispose;
}

static void my_application_init(MyApplication*) {}

MyApplication* my_application_new() {
  g_set_prgname(APPLICATION_ID);
  g_set_application_name("Waddle");
  return MY_APPLICATION(g_object_new(my_application_get_type(),
                                     "application-id", APPLICATION_ID, nullptr));
}
