//! The Calendar destination: an agenda and a month of community events,
//! with each event's details, RSVP and (for who may) editing.
//!
//! What dates mean and how a form becomes a record is decided in
//! `events.rs`; this draws it and wires the sidebar row to the window.

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use chrono::{Local, NaiveDate, Timelike};
use gtk::glib;
use waddle_xmpp_client_ffi::WaddleEvent;

use super::WaddleWindow;
use crate::events::{
    self, agenda, day_heading, detail_label, my_reply, on_day, recurrence_label, replies,
    time_label, undated, Counts, Draft, Occurrence, PlanningRow, Rsvp, When,
};
use crate::session::{Conversation, Flavor, Kind};

/// Days the agenda looks ahead.
const AGENDA_DAYS: i64 = 90;
/// Replies listed in an event's details.
const REPLIES_SHOWN: usize = 50;

/// The window's calendar state.
#[derive(Default)]
pub struct CalendarState {
    open: bool,
    row: Option<adw::ActionRow>,
    ui: Option<CalendarUi>,
    events: Vec<WaddleEvent>,
    loaded: bool,
    failed: Option<String>,
    /// The account may create and change events (the community owner).
    may_edit: bool,
    owner_known: bool,
}

#[derive(Clone)]
struct CalendarUi {
    agenda: gtk::Box,
    month: gtk::Calendar,
    day_list: gtk::Box,
    views: gtk::Stack,
    new_button: gtk::Button,
}

impl WaddleWindow {
    /// The sidebar's Calendar row and the page it opens.
    pub(super) fn setup_calendar(&self) {
        let imp = self.imp();
        let row = adw::ActionRow::builder()
            .title("Calendar")
            .title_lines(1)
            .activatable(true)
            .build();
        row.add_css_class("conversation");
        let icon = gtk::Image::from_icon_name("x-office-calendar-symbolic");
        icon.set_valign(gtk::Align::Center);
        row.add_prefix(&icon);
        imp.calendar_list.append(&row);

        let ui = self.build_calendar_page();
        let mut state = imp.calendar.borrow_mut();
        state.row = Some(row);
        state.ui = Some(ui);
        drop(state);

        // Whatever else takes the content area also takes the selection.
        let window = self.downgrade();
        imp.content_stack
            .connect_visible_child_name_notify(move |stack| {
                if stack.visible_child_name().as_deref() != Some("calendar") {
                    if let Some(window) = window.upgrade() {
                        window.imp().calendar_list.unselect_all();
                    }
                }
            });
    }

    #[must_use]
    pub(super) fn calendar_open(&self) -> bool {
        self.imp().calendar.borrow().open
    }

    /// Forget the account's events, on sign-out.
    pub(super) fn reset_calendar(&self) {
        let imp = self.imp();
        {
            let mut state = imp.calendar.borrow_mut();
            let (row, ui) = (state.row.take(), state.ui.take());
            *state = CalendarState {
                row,
                ui,
                ..CalendarState::default()
            };
        }
        self.leave_calendar();
        self.draw_calendar();
    }

    pub(super) fn show_calendar(&self) {
        let imp = self.imp();
        self.leave_inbox();
        imp.calendar.borrow_mut().open = true;
        // Nothing is being read while the calendar is on screen.
        imp.current.replace(None);
        self.render_conversations();
        if let Some(row) = imp.calendar.borrow().row.as_ref() {
            imp.calendar_list.select_row(Some(row));
        }
        imp.room_title.set_title("Calendar");
        imp.room_page.set_title("Calendar");
        imp.thread_back.set_visible(false);
        imp.load_earlier.set_visible(false);
        imp.content_stack.set_visible_child_name("calendar");
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(false);
        }
        self.cancel_compose();
        self.refresh_composer();
        self.refresh_notify_button();
        self.refresh_message_tools();
        self.refresh_thread_context();
        imp.split_view.set_show_content(true);
        self.draw_calendar();
        self.refresh_calendar();
    }

    /// A conversation is opening: the calendar gives way to it.
    pub(super) fn leave_calendar(&self) {
        let imp = self.imp();
        let was_open = std::mem::take(&mut imp.calendar.borrow_mut().open);
        if !was_open {
            return;
        }
        imp.calendar_list.unselect_all();
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(true);
        }
    }

    /// Fetch the events again.
    pub(super) fn refresh_calendar(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let events = session.events().await;
            // Who may edit changes rarely: ask once per sign-in.
            let known = window.imp().calendar.borrow().owner_known;
            let may_edit = if known {
                None
            } else {
                Some(session.may_edit_events().await)
            };
            let imp = window.imp();
            if imp.generation.get() != generation {
                return;
            }
            {
                let mut state = imp.calendar.borrow_mut();
                match events {
                    Ok(events) => {
                        state.events = events;
                        state.loaded = true;
                        state.failed = None;
                    }
                    Err(message) => state.failed = Some(message),
                }
                if let Some(may_edit) = may_edit {
                    state.may_edit = may_edit;
                    state.owner_known = true;
                }
            }
            window.draw_calendar();
        });
    }

    fn build_calendar_page(&self) -> CalendarUi {
        let imp = self.imp();
        let page = &imp.calendar_page;

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.set_margin_start(16);
        header.set_margin_end(16);
        header.set_margin_top(12);
        let title = gtk::Label::builder()
            .label("Events")
            .hexpand(true)
            .xalign(0.0)
            .build();
        title.add_css_class("title-2");
        header.append(&title);

        let views = gtk::Stack::builder().vexpand(true).build();
        let agenda_button = gtk::ToggleButton::builder()
            .label("Agenda")
            .active(true)
            .build();
        let month_button = gtk::ToggleButton::builder()
            .label("Month")
            .group(&agenda_button)
            .build();
        let switcher = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        switcher.add_css_class("linked");
        switcher.set_valign(gtk::Align::Center);
        switcher.append(&agenda_button);
        switcher.append(&month_button);
        header.append(&switcher);
        let stack = views.clone();
        agenda_button.connect_toggled(move |button| {
            if button.is_active() {
                stack.set_visible_child_name("agenda");
            }
        });
        let stack = views.clone();
        month_button.connect_toggled(move |button| {
            if button.is_active() {
                stack.set_visible_child_name("month");
            }
        });

        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Refresh Events"));
        refresh.set_valign(gtk::Align::Center);
        let window = self.downgrade();
        refresh.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.refresh_calendar();
            }
        });
        header.append(&refresh);

        let new_button = gtk::Button::with_label("New Event");
        new_button.add_css_class("suggested-action");
        new_button.set_valign(gtk::Align::Center);
        new_button.set_visible(false);
        let window = self.downgrade();
        new_button.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.new_event();
            }
        });
        header.append(&new_button);
        page.append(&header);

        let agenda = gtk::Box::new(gtk::Orientation::Vertical, 6);
        views.add_named(&scrolled(&agenda), Some("agenda"));

        let month = gtk::Calendar::new();
        month.add_css_class("card");
        let day_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let month_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        month_page.append(&month);
        month_page.append(&day_list);
        views.add_named(&scrolled(&month_page), Some("month"));
        let window = self.downgrade();
        let changed = move |_: &gtk::Calendar| {
            if let Some(window) = window.upgrade() {
                window.draw_month();
            }
        };
        month.connect_day_selected(changed.clone());
        month.connect_month_notify(changed.clone());
        month.connect_year_notify(changed);

        page.append(&views);
        CalendarUi {
            agenda,
            month,
            day_list,
            views,
            new_button,
        }
    }

    fn calendar_ui(&self) -> Option<CalendarUi> {
        self.imp().calendar.borrow().ui.clone()
    }

    /// Draw both views from the events held.
    pub(super) fn draw_calendar(&self) {
        let Some(ui) = self.calendar_ui() else {
            return;
        };
        ui.new_button
            .set_visible(self.imp().calendar.borrow().may_edit);
        self.draw_agenda(&ui);
        self.draw_month();
    }

    fn draw_agenda(&self, ui: &CalendarUi) {
        clear(&ui.agenda);
        let (events, loaded, failed) = {
            let state = self.imp().calendar.borrow();
            (state.events.clone(), state.loaded, state.failed.clone())
        };
        if let Some(message) = failed.filter(|_| !loaded) {
            ui.agenda.append(&self.failed_page(&message));
            return;
        }
        if !loaded {
            let spinner = adw::Spinner::builder()
                .width_request(32)
                .height_request(32)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .vexpand(true)
                .build();
            ui.agenda.append(&spinner);
            return;
        }
        let today = Local::now().date_naive();
        let groups = agenda(&events, today, AGENDA_DAYS, &Local);
        let dateless = undated(&events);
        if groups.is_empty() && dateless.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("x-office-calendar-symbolic")
                .title("No Upcoming Events")
                .description("Nothing is scheduled in the next three months.")
                .vexpand(true)
                .build();
            ui.agenda.append(&empty);
            return;
        }
        if let Some(message) = self.imp().calendar.borrow().failed.clone() {
            let note = gtk::Label::builder()
                .label(format!("{message} Showing what was loaded before."))
                .xalign(0.0)
                .wrap(true)
                .build();
            note.add_css_class("error");
            ui.agenda.append(&note);
        }
        for group in &groups {
            ui.agenda
                .append(&self.day_section(&day_heading(group.day, today), &group.items));
        }
        if !dateless.is_empty() {
            let items: Vec<Occurrence> = dateless.into_iter().map(Occurrence::undated).collect();
            ui.agenda.append(&self.day_section("No Date Yet", &items));
        }
    }

    fn failed_page(&self, message: &str) -> adw::StatusPage {
        let retry = gtk::Button::with_label("Try Again");
        retry.add_css_class("pill");
        retry.set_halign(gtk::Align::Center);
        let window = self.downgrade();
        retry.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.refresh_calendar();
            }
        });
        adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title("Events Could Not Be Loaded")
            .description(message)
            .child(&retry)
            .vexpand(true)
            .build()
    }

    /// A day's heading over its events.
    fn day_section(&self, heading: &str, items: &[Occurrence]) -> gtk::Box {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 6);
        section.set_margin_top(6);
        let label = gtk::Label::builder().label(heading).xalign(0.0).build();
        label.add_css_class("heading");
        section.append(&label);
        section.append(&self.occurrence_list(items));
        section
    }

    fn occurrence_list(&self, items: &[Occurrence]) -> gtk::ListBox {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for occurrence in items {
            list.append(&self.occurrence_row(occurrence));
        }
        list
    }

    fn occurrence_row(&self, occurrence: &Occurrence) -> adw::ActionRow {
        let mut subtitle = time_label(occurrence, &Local);
        if let Some(place) = occurrence
            .location
            .as_deref()
            .filter(|place| !place.is_empty())
        {
            subtitle.push_str(" \u{b7} ");
            subtitle.push_str(place);
        }
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&occurrence.title))
            .subtitle(glib::markup_escape_text(&subtitle))
            .activatable(true)
            .build();
        if occurrence.repeating() {
            let repeat = gtk::Image::from_icon_name("media-playlist-repeat-symbolic");
            repeat.set_tooltip_text(Some("Repeats"));
            row.add_suffix(&repeat);
        }
        if let Some(event) = self.event_by_id(&occurrence.event_id) {
            let me = self.my_jid();
            if let Some(reply) = my_reply(&event.attendees, &me) {
                let mine = gtk::Label::new(Some(reply.label()));
                mine.add_css_class("accent");
                mine.add_css_class("caption");
                row.add_suffix(&mine);
            }
            let counts = Counts::of(&event.attendees);
            if counts.going > 0 {
                let going = gtk::Label::new(Some(&format!("{} going", counts.going)));
                going.add_css_class("dim-label");
                going.add_css_class("caption");
                row.add_suffix(&going);
            }
        }
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let (window, occurrence) = (self.downgrade(), occurrence.clone());
        row.connect_activated(move |_| {
            if let Some(window) = window.upgrade() {
                window.show_event(&occurrence);
            }
        });
        row
    }

    /// Mark the calendar's month and list the selected day.
    fn draw_month(&self) {
        let Some(ui) = self.calendar_ui() else {
            return;
        };
        let events = self.imp().calendar.borrow().events.clone();
        let date = ui.month.date();
        ui.month.clear_marks();
        if let Ok(month) = u32::try_from(date.month()) {
            for day in events::marked_days(&events, date.year(), month, &Local) {
                ui.month.mark_day(day);
            }
        }
        clear(&ui.day_list);
        let Some(day) = selected_day(&ui.month) else {
            return;
        };
        let items = on_day(&events, day, &Local);
        let heading = gtk::Label::builder()
            .label(day_heading(day, Local::now().date_naive()))
            .xalign(0.0)
            .build();
        heading.add_css_class("heading");
        ui.day_list.append(&heading);
        if items.is_empty() {
            let none = gtk::Label::builder()
                .label("Nothing is scheduled.")
                .xalign(0.0)
                .build();
            none.add_css_class("dim-label");
            ui.day_list.append(&none);
        } else {
            ui.day_list.append(&self.occurrence_list(&items));
        }
    }

    pub(super) fn event_by_id(&self, id: &str) -> Option<WaddleEvent> {
        self.imp()
            .calendar
            .borrow()
            .events
            .iter()
            .find(|event| event.id == id)
            .cloned()
    }

    pub(super) fn my_jid(&self) -> String {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .map(|session| session.me.bare.to_string())
            .unwrap_or_default()
    }

    fn may_edit_events(&self) -> bool {
        self.imp().calendar.borrow().may_edit
    }

    /// A new event, on the day the month shows (or today).
    fn new_event(&self) {
        let day = self
            .calendar_ui()
            .filter(|ui| ui.views.visible_child_name().as_deref() == Some("month"))
            .and_then(|ui| selected_day(&ui.month))
            .unwrap_or_else(|| Local::now().date_naive());
        self.open_event_form(Draft::new(day, Local::now().hour() + 1));
    }

    /// The event was saved: show it, replacing what was held.
    pub(super) fn event_saved(&self, event: WaddleEvent) {
        {
            let mut state = self.imp().calendar.borrow_mut();
            match state.events.iter_mut().find(|held| held.id == event.id) {
                Some(held) => *held = event,
                None => state.events.push(event),
            }
        }
        self.draw_calendar();
    }

    fn event_gone(&self, id: &str) {
        self.imp()
            .calendar
            .borrow_mut()
            .events
            .retain(|event| event.id != id);
        self.draw_calendar();
    }

    /// An event's details, with its answers and, for who may, its controls.
    fn show_event(&self, occurrence: &Occurrence) {
        let body = adw::Bin::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&body));
        let dialog = adw::Dialog::builder()
            .title("Event")
            .content_width(480)
            .content_height(640)
            .child(&toolbar)
            .build();
        self.fill_event(&dialog, &body, occurrence);
        dialog.present(Some(self));
    }

    fn fill_event(&self, dialog: &adw::Dialog, body: &adw::Bin, occurrence: &Occurrence) {
        let Some(event) = self.event_by_id(&occurrence.event_id) else {
            dialog.close();
            return;
        };
        let may_edit = self.may_edit_events();
        let me = self.my_jid();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.set_margin_top(12);
        content.set_margin_bottom(18);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let title = gtk::Label::builder()
            .label(&occurrence.title)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        title.add_css_class("title-1");
        content.append(&title);
        content.append(&info_line(
            "x-office-calendar-symbolic",
            &detail_label(occurrence, &Local),
        ));
        if let Some(rule) = event.recurrence.as_ref() {
            content.append(&info_line(
                "media-playlist-repeat-symbolic",
                &recurrence_label(rule),
            ));
        }
        if let Some(place) = occurrence
            .location
            .as_deref()
            .filter(|place| !place.is_empty())
        {
            content.append(&info_line("mark-location-symbolic", place));
        }
        if let Some(text) = occurrence
            .description
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            let about = gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .build();
            content.append(&about);
        }

        content.append(&self.answers_section(dialog, body, occurrence, &event, &me));
        content.append(&self.room_section(dialog, occurrence, &event, may_edit));
        if may_edit {
            content.append(&self.controls_section(dialog, occurrence, &event));
        }

        body.set_child(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(
                    &adw::Clamp::builder()
                        .maximum_size(560)
                        .child(&content)
                        .build(),
                )
                .build(),
        ));
    }

    fn answers_section(
        &self,
        dialog: &adw::Dialog,
        body: &adw::Bin,
        occurrence: &Occurrence,
        event: &WaddleEvent,
        me: &str,
    ) -> gtk::Box {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let heading = gtk::Label::builder()
            .label("Are you going?")
            .xalign(0.0)
            .build();
        heading.add_css_class("heading");
        section.append(&heading);
        let mine = my_reply(&event.attendees, me);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.add_css_class("linked");
        let mut first: Option<gtk::ToggleButton> = None;
        for reply in Rsvp::ALL {
            let button = gtk::ToggleButton::builder()
                .label(reply.label())
                .hexpand(true)
                .active(mine == Some(reply))
                .build();
            if let Some(first) = first.as_ref() {
                button.set_group(Some(first));
            } else {
                first = Some(button.clone());
            }
            let (window, dialog, body, occurrence) = (
                self.downgrade(),
                dialog.clone(),
                body.clone(),
                occurrence.clone(),
            );
            button.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    window.send_reply(&dialog, &body, &occurrence, reply);
                }
            });
            buttons.append(&button);
        }
        section.append(&buttons);
        if event.recurrence.is_some() {
            let note = gtk::Label::builder()
                .label("Your answer covers every date of the series.")
                .xalign(0.0)
                .build();
            note.add_css_class("dim-label");
            note.add_css_class("caption");
            section.append(&note);
        }
        let counts = gtk::Label::builder()
            .label(Counts::of(&event.attendees).summary())
            .xalign(0.0)
            .build();
        counts.add_css_class("dim-label");
        section.append(&counts);
        let answered = replies(&event.attendees, me);
        if !answered.is_empty() {
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for (jid, reply) in answered.iter().take(REPLIES_SHOWN) {
                let name = if jid.eq_ignore_ascii_case(me) {
                    "You".to_string()
                } else {
                    jid.clone()
                };
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&name))
                    .build();
                let answer = gtk::Label::new(Some(reply.label()));
                answer.add_css_class("dim-label");
                row.add_suffix(&answer);
                list.append(&row);
            }
            section.append(&list);
        }
        section
    }

    fn send_reply(
        &self,
        dialog: &adw::Dialog,
        body: &adw::Bin,
        occurrence: &Occurrence,
        reply: Rsvp,
    ) {
        let Some(session) = self.imp().session.borrow().clone() else {
            return;
        };
        let me = self.my_jid();
        if self
            .event_by_id(&occurrence.event_id)
            .is_some_and(|event| my_reply(&event.attendees, &me) == Some(reply))
        {
            return;
        }
        let (window, dialog, body, occurrence) = (
            self.clone(),
            dialog.clone(),
            body.clone(),
            occurrence.clone(),
        );
        let generation = self.imp().generation.get();
        glib::spawn_future_local(async move {
            let sent = session.rsvp(occurrence.uid.clone(), reply).await;
            if window.imp().generation.get() != generation {
                return;
            }
            match sent {
                Ok(()) => {
                    {
                        let mut state = window.imp().calendar.borrow_mut();
                        if let Some(event) = state
                            .events
                            .iter_mut()
                            .find(|event| event.id == occurrence.event_id)
                        {
                            events::apply_reply(event, &me, reply);
                        }
                    }
                    window.draw_calendar();
                }
                Err(message) => window.toast(&message),
            }
            // Redraw the answers either way: a failed pick returns to the old one.
            window.fill_event(&dialog, &body, &occurrence);
        });
    }

    fn room_section(
        &self,
        dialog: &adw::Dialog,
        occurrence: &Occurrence,
        event: &WaddleEvent,
        may_edit: bool,
    ) -> gtk::Box {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        if let Some(room) = event.workspace_room.clone() {
            let heading = gtk::Label::builder()
                .label("Event Room")
                .xalign(0.0)
                .build();
            heading.add_css_class("heading");
            section.append(&heading);
            let open = gtk::Button::with_label("Open Room");
            open.set_halign(gtk::Align::Start);
            let (window, dialog, name) =
                (self.downgrade(), dialog.clone(), occurrence.title.clone());
            open.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    dialog.close();
                    window.open_event_room(&room, &name);
                }
            });
            section.append(&open);
            self.planning_section(&section, &event.workspace_room.clone().unwrap_or_default());
        } else if may_edit {
            let create = gtk::Button::with_label("Create Room");
            create.set_halign(gtk::Align::Start);
            create.set_tooltip_text(Some("Create a room for this event to talk in"));
            let (window, dialog, id) = (
                self.downgrade(),
                dialog.clone(),
                occurrence.event_id.clone(),
            );
            create.connect_clicked(move |button| {
                button.set_sensitive(false);
                if let Some(window) = window.upgrade() {
                    window.create_room_for(&dialog, &id);
                }
            });
            section.append(&create);
        }
        section
    }

    /// The room's planning items, read only, once they arrive.
    fn planning_section(&self, section: &gtk::Box, room: &str) {
        let Some(session) = self.imp().session.borrow().clone() else {
            return;
        };
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 6);
        section.append(&holder);
        let (room, generation) = (room.to_string(), self.imp().generation.get());
        let (window, holder) = (self.clone(), holder);
        glib::spawn_future_local(async move {
            let items = session.planning_items(room).await;
            // The details were closed or redrawn, or the account changed.
            if window.imp().generation.get() != generation || holder.root().is_none() {
                return;
            }
            let Ok(items) = items else {
                return;
            };
            let rows = events::planning_rows(&items);
            if rows.is_empty() {
                return;
            }
            let heading = gtk::Label::builder().label("Planning").xalign(0.0).build();
            heading.add_css_class("heading");
            holder.append(&heading);
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for row in &rows {
                list.append(&planning_row(row));
            }
            holder.append(&list);
        });
    }

    fn controls_section(
        &self,
        dialog: &adw::Dialog,
        occurrence: &Occurrence,
        event: &WaddleEvent,
    ) -> gtk::Box {
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.set_margin_top(6);
        let edit = gtk::Button::with_label("Edit Event");
        let (window, dialog_edit, event_edit) = (self.downgrade(), dialog.clone(), event.clone());
        edit.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                dialog_edit.close();
                window.open_event_form(Draft::from_event(&event_edit, &Local));
            }
        });
        controls.append(&edit);
        if let (true, Some(start)) = (occurrence.repeating(), occurrence.recurrence_id) {
            let cancel = gtk::Button::with_label("Cancel This Date");
            let (window, dialog, id) = (
                self.downgrade(),
                dialog.clone(),
                occurrence.event_id.clone(),
            );
            cancel.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    window.confirm_cancel_date(&dialog, &id, start);
                }
            });
            controls.append(&cancel);
        }
        let delete = gtk::Button::with_label("Delete Event");
        delete.add_css_class("destructive-action");
        let (window, dialog, id) = (
            self.downgrade(),
            dialog.clone(),
            occurrence.event_id.clone(),
        );
        delete.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.confirm_delete_event(&dialog, &id);
            }
        });
        controls.append(&delete);
        controls
    }

    fn confirm_cancel_date(&self, details: &adw::Dialog, id: &str, start: When) {
        let alert = adw::AlertDialog::new(
            Some("Cancel This Date?"),
            Some("Only this date is cancelled; the rest of the series stays."),
        );
        alert.add_responses(&[("keep", "Keep"), ("cancel", "Cancel Date")]);
        alert.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("keep"));
        alert.set_close_response("keep");
        let (window, details, id) = (self.clone(), details.clone(), id.to_string());
        alert.connect_response(Some("cancel"), move |_, _| {
            let Some(session) = window.imp().session.borrow().clone() else {
                return;
            };
            let (window, details, id) = (window.clone(), details.clone(), id.clone());
            let generation = window.imp().generation.get();
            glib::spawn_future_local(async move {
                let result = session.cancel_occurrence(id, start).await;
                if window.imp().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(event) => {
                        details.close();
                        window.event_saved(event);
                        window.toast("That date is cancelled.");
                    }
                    Err(message) => window.toast(&message),
                }
            });
        });
        alert.present(Some(self));
    }

    fn confirm_delete_event(&self, details: &adw::Dialog, id: &str) {
        let alert = adw::AlertDialog::new(
            Some("Delete Event?"),
            Some("The event and every date of it are removed for everyone."),
        );
        alert.add_responses(&[("keep", "Keep"), ("delete", "Delete")]);
        alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("keep"));
        alert.set_close_response("keep");
        let (window, details, id) = (self.clone(), details.clone(), id.to_string());
        alert.connect_response(Some("delete"), move |_, _| {
            let Some(session) = window.imp().session.borrow().clone() else {
                return;
            };
            let (window, details, id) = (window.clone(), details.clone(), id.clone());
            let generation = window.imp().generation.get();
            glib::spawn_future_local(async move {
                let result = session.retract_event(id.clone()).await;
                if window.imp().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        details.close();
                        window.event_gone(&id);
                        window.toast("The event is deleted.");
                    }
                    Err(message) => window.toast(&message),
                }
            });
        });
        alert.present(Some(self));
    }

    /// Create a room for the event and link it to the event.
    fn create_room_for(&self, details: &adw::Dialog, id: &str) {
        let Some(event) = self.event_by_id(id) else {
            return;
        };
        let Some(session) = self.imp().session.borrow().clone() else {
            return;
        };
        let (window, details) = (self.clone(), details.clone());
        let generation = self.imp().generation.get();
        glib::spawn_future_local(async move {
            let room = session.create_event_room(event.summary.clone()).await;
            if window.imp().generation.get() != generation {
                return;
            }
            let room = match room {
                Ok(room) => room,
                Err(message) => {
                    window.toast(&message);
                    details.close();
                    return;
                }
            };
            let mut draft = Draft::from_event(&event, &Local);
            draft.room = Some(room.clone());
            let linked = match draft.to_event(&Local) {
                Ok(record) => session.publish_event(record).await,
                Err(error) => Err(error.to_string()),
            };
            if window.imp().generation.get() != generation {
                return;
            }
            details.close();
            match linked {
                Ok(saved) => {
                    window.event_saved(saved);
                    window.open_event_room(&room, &event.summary);
                }
                Err(message) => window.toast(&format!(
                    "The room was created but could not be linked to the event. {message}"
                )),
            }
        });
    }

    /// Go to the event's room, joining it if the sidebar does not list it.
    fn open_event_room(&self, room: &str, name: &str) {
        let imp = self.imp();
        let known = imp
            .conversations
            .borrow()
            .iter()
            .any(|conversation| conversation.key() == room);
        if !known {
            imp.conversations.borrow_mut().push(Conversation {
                jid: room.to_string(),
                name: name.to_string(),
                kind: Kind::Room,
                flavor: Flavor::Plain,
                autojoin: false,
                thread: None,
            });
        }
        self.open_conversation(room);
    }
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    child.set_margin_start(16);
    child.set_margin_end(16);
    child.set_margin_top(12);
    child.set_margin_bottom(16);
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(720).child(child).build())
        .build()
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn info_line(icon: &str, text: &str) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let image = gtk::Image::from_icon_name(icon);
    image.set_valign(gtk::Align::Start);
    image.add_css_class("dim-label");
    line.append(&image);
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .hexpand(true)
        .build();
    line.append(&label);
    line
}

fn planning_row(row: &PlanningRow) -> adw::ActionRow {
    let widget = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&row.title))
        .subtitle(glib::markup_escape_text(row.kind))
        .build();
    if row.done {
        widget.add_prefix(&gtk::Image::from_icon_name("object-select-symbolic"));
    }
    if let Some(note) = row.note.as_deref() {
        let label = gtk::Label::new(Some(note));
        label.add_css_class("dim-label");
        widget.add_suffix(&label);
    }
    widget
}

/// The day a gtk calendar has selected.
fn selected_day(calendar: &gtk::Calendar) -> Option<NaiveDate> {
    let date = calendar.date();
    NaiveDate::from_ymd_opt(
        date.year(),
        u32::try_from(date.month()).ok()?,
        u32::try_from(date.day_of_month()).ok()?,
    )
}
