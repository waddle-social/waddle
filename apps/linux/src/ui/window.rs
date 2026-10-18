use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};

use crate::auth::device::{DeviceAuthClient, DeviceAuthorization, Provider, ServerUrl};
use crate::auth::store::{CredentialStore, StoredAccount};
use crate::runtime;
use crate::session::{Conversation, Session};
use waddle_client_session::Timeline;

use super::actions::Compose;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "data/ui/window.blp")]
    pub struct WaddleWindow {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub server_entry: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub server_continue: TemplateChild<gtk::Button>,
        #[template_child]
        pub providers_status: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub provider_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub user_code_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub account_status: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub account_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub room_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub room_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub room_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub timeline_scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub timeline_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub composer: TemplateChild<gtk::Box>,
        #[template_child]
        pub composer_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub composer_context: TemplateChild<gtk::Box>,
        #[template_child]
        pub composer_context_label: TemplateChild<gtk::Label>,

        pub auth_client: RefCell<Option<DeviceAuthClient>>,
        pub providers: RefCell<Vec<Provider>>,
        pub authorization: RefCell<Option<DeviceAuthorization>>,
        pub account: RefCell<Option<StoredAccount>>,
        /// Bumped whenever a sign-in attempt is abandoned, so a poll that
        /// completes late cannot sign in a flow the user cancelled.
        pub attempt: Cell<u64>,

        // The signed-in session (see ui/conversation.rs).
        pub session: RefCell<Option<Session>>,
        /// Bumped on sign-out so events from an old session are ignored.
        pub generation: Cell<u64>,
        pub connected: Cell<bool>,
        pub reconnect_attempt: Cell<u32>,
        /// Rooms in catalogue order, then DMs.
        pub conversations: RefCell<Vec<Conversation>>,
        pub unread: RefCell<HashMap<String, u32>>,
        /// One timeline per conversation, by bare JID.
        pub timelines: RefCell<HashMap<String, Timeline>>,
        /// The open conversation's bare JID.
        pub current: RefCell<Option<String>>,
        /// A conversation asked for (from a notification) before the
        /// session had loaded it.
        pub pending_open: RefCell<Option<String>>,
        /// Conversations to mark read on the server shortly.
        pub read_pending: RefCell<HashSet<String>>,
        pub compose: RefCell<Compose>,
        pub loading_history: Cell<bool>,
        pub rows: RefCell<Option<gio::ListStore>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WaddleWindow {
        const NAME: &'static str = "WaddleWindow";
        type Type = super::WaddleWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_instance_callbacks();
            klass.install_action_async("win.sign-out", None, |window, _, _| async move {
                window.sign_out().await;
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for WaddleWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let window = self.obj().clone();
            window.setup_timeline();
            glib::spawn_future_local(async move { window.restore().await });
        }
    }

    impl WidgetImpl for WaddleWindow {}
    impl WindowImpl for WaddleWindow {}
    impl ApplicationWindowImpl for WaddleWindow {}
    impl AdwApplicationWindowImpl for WaddleWindow {}
}

glib::wrapper! {
    pub struct WaddleWindow(ObjectSubclass<imp::WaddleWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
            gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

#[gtk::template_callbacks]
impl WaddleWindow {
    pub fn new(app: &impl IsA<gtk::Application>) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    #[template_callback]
    fn on_server_continue(&self) {
        let imp = self.imp();
        let server = match ServerUrl::parse(&imp.server_entry.text()) {
            Ok(server) => server,
            Err(error) => return self.toast(&error.to_string()),
        };
        let http = match http_client() {
            Ok(http) => http,
            Err(error) => return self.toast(&error.to_string()),
        };
        let client = DeviceAuthClient::new(http, server);
        imp.server_continue.set_sensitive(false);
        let window = self.clone();
        glib::spawn_future_local(async move {
            let lookup = client.clone();
            let result = runtime::spawn(async move { lookup.providers().await }).await;
            window.imp().server_continue.set_sensitive(true);
            match result {
                Ok(Ok(providers)) if providers.is_empty() => {
                    window.toast("This server offers no way to sign in.")
                }
                Ok(Ok(providers)) => window.show_providers(client, providers),
                Ok(Err(error)) => window.toast(&error.to_string()),
                Err(_) => window.toast("Sign-in stopped unexpectedly."),
            }
        });
    }

    #[template_callback]
    fn on_back_to_server(&self) {
        let imp = self.imp();
        imp.attempt.set(imp.attempt.get().wrapping_add(1));
        imp.authorization.replace(None);
        imp.stack.set_visible_child_name("server");
    }

    #[template_callback]
    fn on_provider_activated(&self, row: &gtk::ListBoxRow) {
        let imp = self.imp();
        let Ok(index) = usize::try_from(row.index()) else {
            return;
        };
        let (Some(provider), Some(client)) = (
            imp.providers.borrow().get(index).cloned(),
            imp.auth_client.borrow().clone(),
        ) else {
            return;
        };
        imp.attempt.set(imp.attempt.get().wrapping_add(1));
        let attempt = imp.attempt.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let starter = client.clone();
            let started = runtime::spawn(async move { starter.start(&provider).await }).await;
            let authorization = match started {
                Ok(Ok(authorization)) => authorization,
                Ok(Err(error)) => return window.toast(&error.to_string()),
                Err(_) => return window.toast("Sign-in stopped unexpectedly."),
            };
            if window.imp().attempt.get() != attempt {
                return;
            }
            window.show_code(&authorization);

            let poller = client.clone();
            let pending = authorization.clone();
            let approved =
                runtime::spawn(async move { poller.wait_for_approval(&pending).await }).await;
            if window.imp().attempt.get() != attempt {
                return;
            }
            match approved {
                Ok(Ok(session)) => {
                    let account = StoredAccount::new(client.server().clone(), session);
                    window.save_and_enter(account).await;
                }
                Ok(Err(error)) => {
                    window.toast(&error.to_string());
                    window.imp().stack.set_visible_child_name("server");
                }
                Err(_) => window.toast("Sign-in stopped unexpectedly."),
            }
        });
    }

    #[template_callback]
    fn on_room_activated(&self, row: &gtk::ListBoxRow) {
        if let Some(conversation) = self.conversation_at(row.index()) {
            self.open_conversation(&conversation.jid);
        }
    }

    #[template_callback]
    fn on_cancel_compose(&self) {
        self.cancel_compose();
    }

    #[template_callback]
    fn on_send(&self) {
        self.send_current();
    }

    #[template_callback]
    fn on_open_verification(&self) {
        if let Some(authorization) = self.imp().authorization.borrow().as_ref() {
            self.open_verification(authorization);
        }
    }

    /// Show the first stored account, or the sign-in steps when there is none.
    async fn restore(&self) {
        let stored = runtime::spawn(async {
            let store = CredentialStore::open().await?;
            store.accounts().await
        })
        .await;
        match stored {
            Ok(Ok(accounts)) => match accounts.into_iter().next() {
                Some(account) => self.enter(account),
                None => self.imp().stack.set_visible_child_name("server"),
            },
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not read stored accounts");
                self.toast(&error.to_string());
                self.imp().stack.set_visible_child_name("server");
            }
            Err(_) => self.imp().stack.set_visible_child_name("server"),
        }
    }

    fn show_providers(&self, client: DeviceAuthClient, providers: Vec<Provider>) {
        let imp = self.imp();
        while let Some(row) = imp.provider_list.first_child() {
            imp.provider_list.remove(&row);
        }
        for provider in &providers {
            let row = adw::ActionRow::builder()
                .title(provider.display_name.as_str())
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            imp.provider_list.append(&row);
        }
        imp.providers_status
            .set_description(Some(&format!("Signing in to {}", client.server())));
        imp.providers.replace(providers);
        imp.auth_client.replace(Some(client));
        imp.stack.set_visible_child_name("providers");
    }

    fn show_code(&self, authorization: &DeviceAuthorization) {
        let imp = self.imp();
        imp.user_code_label.set_label(&authorization.user_code);
        imp.authorization.replace(Some(authorization.clone()));
        imp.stack.set_visible_child_name("code");
        self.open_verification(authorization);
    }

    fn open_verification(&self, authorization: &DeviceAuthorization) {
        let window = self.clone();
        gtk::UriLauncher::new(authorization.verification_url.as_str()).launch(
            Some(self),
            None::<&gio::Cancellable>,
            move |result| {
                if let Err(error) = result {
                    tracing::warn!(%error, "could not open the sign-in page");
                    window.toast(
                        "Could not open the browser. Open the sign-in page from this window.",
                    );
                }
            },
        );
    }

    async fn save_and_enter(&self, account: StoredAccount) {
        let to_store = account.clone();
        let saved = runtime::spawn(async move {
            let store = CredentialStore::open().await?;
            store.save(&to_store).await
        })
        .await;
        if let Ok(Err(error)) = saved {
            // Signed in for this run; it will ask again next time.
            tracing::warn!(%error, "could not save the session");
            self.toast("Signed in, but the session could not be saved to the keyring.");
        }
        self.enter(account);
    }

    fn enter(&self, account: StoredAccount) {
        let imp = self.imp();
        imp.account_label
            .set_label(&format!("{} · {}", account.session.jid, account.server));
        imp.account.replace(Some(account.clone()));
        imp.stack.set_visible_child_name("shell");
        self.start_session(&account);
    }

    async fn sign_out(&self) {
        let Some(account) = self.imp().account.take() else {
            return;
        };
        self.stop_session().await;
        let removed = runtime::spawn(async move {
            let store = CredentialStore::open().await?;
            store.remove(&account).await
        })
        .await;
        if let Ok(Err(error)) = removed {
            tracing::warn!(%error, "could not remove the stored session");
            self.toast(&error.to_string());
        }
        self.imp().stack.set_visible_child_name("server");
    }

    pub(super) fn toast(&self, message: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(message));
    }
}

fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(concat!("Waddle-Linux/", env!("CARGO_PKG_VERSION")))
        .build()
}
