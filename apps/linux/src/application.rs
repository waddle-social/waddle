use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};

use crate::ui::WaddleWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct WaddleApplication;

    #[glib::object_subclass]
    impl ObjectSubclass for WaddleApplication {
        const NAME: &'static str = "WaddleApplication";
        type Type = super::WaddleApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for WaddleApplication {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup_actions();
        }
    }

    impl ApplicationImpl for WaddleApplication {
        fn activate(&self) {
            self.obj().window().present();
        }
    }

    impl GtkApplicationImpl for WaddleApplication {}
    impl AdwApplicationImpl for WaddleApplication {}
}

glib::wrapper! {
    pub struct WaddleApplication(ObjectSubclass<imp::WaddleApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl WaddleApplication {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", crate::APP_ID)
            .property("flags", gio::ApplicationFlags::default())
            .build()
    }

    fn setup_actions(&self) {
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &Self, _, _| app.show_about())
            .build();
        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &Self, _, _| app.quit())
            .build();
        // A notification's default action: show that conversation.
        let open = gio::ActionEntry::builder("open-conversation")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|app: &Self, _, target| {
                let Some(jid) = target.and_then(|target| target.get::<String>()) else {
                    return;
                };
                let window = app.window();
                window.present();
                window.open_conversation(&jid);
            })
            .build();
        self.add_action_entries([about, quit, open]);
        self.set_accels_for_action("app.quit", &["<Control>q"]);
    }

    /// The one window, made on first use. It hides on close, so it
    /// outlives being closed.
    fn window(&self) -> WaddleWindow {
        self.windows()
            .into_iter()
            .find_map(|window| window.downcast::<WaddleWindow>().ok())
            .unwrap_or_else(|| WaddleWindow::new(self))
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Waddle")
            .application_icon(crate::APP_ID)
            .developer_name("Waddle Contributors")
            .version(env!("CARGO_PKG_VERSION"))
            .website("https://waddle.social")
            .issue_url("https://github.com/waddle-social/waddle/issues")
            .license_type(gtk::License::Agpl30)
            .build();
        about.present(self.active_window().as_ref());
    }
}

impl Default for WaddleApplication {
    fn default() -> Self {
        Self::new()
    }
}
