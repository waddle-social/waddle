//! The form that creates or edits an event.
//!
//! Fields write straight into an `events::Draft`; what that may hold and
//! the record it becomes are decided there.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use chrono::{Datelike, Local, NaiveDate, NaiveTime, Timelike};
use gtk::glib;

use super::WaddleWindow;
use crate::events::{weekday_name, weekday_of, Draft, Ending, Repeat};
use waddle_xmpp_client_ffi::WaddleWeekday;

type Shared = Rc<RefCell<Draft>>;
/// What the start's pickers call to move the end's pickers after it.
type Follow = Rc<RefCell<Option<Rc<dyn Fn(&Draft)>>>>;

const WEEK: [WaddleWeekday; 7] = [
    WaddleWeekday::Monday,
    WaddleWeekday::Tuesday,
    WaddleWeekday::Wednesday,
    WaddleWeekday::Thursday,
    WaddleWeekday::Friday,
    WaddleWeekday::Saturday,
    WaddleWeekday::Sunday,
];

/// A date chosen from a calendar popover.
struct DatePicker {
    button: gtk::MenuButton,
    set: Rc<dyn Fn(NaiveDate)>,
}

fn date_text(day: NaiveDate) -> String {
    day.format("%a, %b %-d, %Y").to_string()
}

fn date_picker(initial: NaiveDate, on_change: impl Fn(NaiveDate) + 'static) -> DatePicker {
    let button = gtk::MenuButton::builder()
        .label(date_text(initial))
        .valign(gtk::Align::Center)
        .build();
    let calendar = gtk::Calendar::new();
    let popover = gtk::Popover::builder().child(&calendar).build();
    button.set_popover(Some(&popover));
    let set = {
        let (button, calendar) = (button.clone(), calendar.clone());
        Rc::new(move |day: NaiveDate| {
            button.set_label(&date_text(day));
            if let Ok(date) = glib::DateTime::from_local(
                day.year(),
                i32::try_from(day.month()).unwrap_or(1),
                i32::try_from(day.day()).unwrap_or(1),
                0,
                0,
                0.0,
            ) {
                calendar.select_day(&date);
            }
        })
    };
    set(initial);
    let shown = Rc::new(std::cell::Cell::new((initial.year(), initial.month())));
    calendar.connect_day_selected({
        let (button, popover) = (button.clone(), popover);
        move |calendar| {
            let date = calendar.date();
            let Some(day) = u32::try_from(date.month())
                .ok()
                .zip(u32::try_from(date.day_of_month()).ok())
                .and_then(|(month, day)| NaiveDate::from_ymd_opt(date.year(), month, day))
            else {
                return;
            };
            button.set_label(&date_text(day));
            // Paging to another month also selects a day; only a pick within
            // the month on show closes the popover.
            let month = (day.year(), day.month());
            if shown.replace(month) == month {
                popover.popdown();
            }
            on_change(day);
        }
    });
    DatePicker { button, set }
}

/// Hour and minute spinners.
struct TimePicker {
    widget: gtk::Box,
    set: Rc<dyn Fn(NaiveTime)>,
}

fn time_picker(initial: NaiveTime, on_change: impl Fn(NaiveTime) + 'static) -> TimePicker {
    let hours = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    let minutes = gtk::SpinButton::with_range(0.0, 59.0, 5.0);
    for spin in [&hours, &minutes] {
        spin.set_wrap(true);
        spin.set_numeric(true);
        spin.set_valign(gtk::Align::Center);
        spin.connect_output(|spin| {
            spin.set_text(&format!("{:02}", spin.value_as_int()));
            glib::Propagation::Stop
        });
    }
    let set = {
        let (hours, minutes) = (hours.clone(), minutes.clone());
        Rc::new(move |time: NaiveTime| {
            hours.set_value(f64::from(time.hour()));
            minutes.set_value(f64::from(time.minute()));
        })
    };
    set(initial);
    let read = {
        let (hours, minutes) = (hours.clone(), minutes.clone());
        move || {
            NaiveTime::from_hms_opt(
                u32::try_from(hours.value_as_int()).unwrap_or(0),
                u32::try_from(minutes.value_as_int()).unwrap_or(0),
                0,
            )
            .unwrap_or(NaiveTime::MIN)
        }
    };
    let on_change = Rc::new(on_change);
    for spin in [&hours, &minutes] {
        let (read, on_change) = (read.clone(), on_change.clone());
        spin.connect_value_changed(move |_| on_change(read()));
    }
    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    widget.set_valign(gtk::Align::Center);
    widget.append(&hours);
    widget.append(&gtk::Label::new(Some(":")));
    widget.append(&minutes);
    TimePicker { widget, set }
}

/// A row with a date and, optionally, a time on its right.
fn moment_row(title: &str, date: &DatePicker, time: &TimePicker) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(&date.button);
    row.add_suffix(&time.widget);
    row
}

fn combo(title: &str, options: &[&str], selected: u32) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(options))
        .build();
    row.set_selected(selected);
    row
}

fn spin(title: &str, subtitle: &str, value: u32, max: f64) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(1.0, max, 1.0);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(f64::from(value.max(1)));
    row
}

fn repeat_index(repeat: Repeat) -> u32 {
    Repeat::ALL
        .iter()
        .position(|candidate| *candidate == repeat)
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or(0)
}

fn ending_index(ending: Ending) -> u32 {
    match ending {
        Ending::Never => 0,
        Ending::After(_) => 1,
        Ending::On(_) => 2,
    }
}

impl WaddleWindow {
    /// Open the form for `draft` (a new event when its id is empty).
    pub(super) fn open_event_form(&self, draft: Draft) {
        let Some(session) = self.imp().session.borrow().clone() else {
            return;
        };
        let editing = !draft.event_id.is_empty();
        let state: Shared = Rc::new(RefCell::new(draft));
        let initial = state.borrow().clone();

        // What and where.
        let what = adw::PreferencesGroup::new();
        let title = adw::EntryRow::builder()
            .title("Title")
            .text(&initial.title)
            .build();
        let place = adw::EntryRow::builder()
            .title("Location")
            .text(&initial.location)
            .build();
        what.add(&title);
        what.add(&place);
        let details = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .build();
        details.buffer().set_text(&initial.details);
        let notes = gtk::ScrolledWindow::builder()
            .min_content_height(96)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&details)
            .build();
        notes.add_css_class("card");
        let notes_group = adw::PreferencesGroup::builder()
            .title("Description")
            .build();
        notes_group.add(&notes);

        // When.
        let when = adw::PreferencesGroup::new();
        let all_day = adw::SwitchRow::builder()
            .title("All day")
            .active(initial.all_day)
            .build();
        let has_end = adw::SwitchRow::builder()
            .title("Set an end")
            .active(initial.has_end)
            .build();
        // Moving the start takes the end along; this writes the end's pickers.
        let follow: Follow = Rc::new(RefCell::new(None));
        let moved = {
            let (state, follow) = (state.clone(), follow.clone());
            Rc::new(move |date: Option<NaiveDate>, time: Option<NaiveTime>| {
                {
                    let mut draft = state.borrow_mut();
                    let (date, time) = (
                        date.unwrap_or(draft.start_date),
                        time.unwrap_or(draft.start_time),
                    );
                    draft.move_start(date, time);
                }
                if let Some(follow) = follow.borrow().as_ref() {
                    follow(&state.borrow());
                }
            })
        };
        let start_time = time_picker(initial.start_time, {
            let moved = moved.clone();
            move |time| moved(None, Some(time))
        });
        let start_date = date_picker(initial.start_date, {
            let moved = moved.clone();
            move |day| moved(Some(day), None)
        });
        let end_time = time_picker(initial.end_time, {
            let state = state.clone();
            move |time| state.borrow_mut().end_time = time
        });
        let end_picker = date_picker(initial.end_date, {
            let state = state.clone();
            move |day| state.borrow_mut().end_date = day
        });
        follow.replace(Some({
            let (set_date, set_time) = (end_picker.set.clone(), end_time.set.clone());
            Rc::new(move |draft: &Draft| {
                set_date(draft.end_date);
                set_time(draft.end_time);
            })
        }));
        let start_row = moment_row("Starts", &start_date, &start_time);
        let end_row = moment_row("Ends", &end_picker, &end_time);
        when.add(&all_day);
        when.add(&start_row);
        when.add(&has_end);
        when.add(&end_row);

        // Repeat.
        let repeat_group = adw::PreferencesGroup::new();
        let repeat = combo(
            "Repeat",
            &Repeat::ALL.map(Repeat::label),
            repeat_index(initial.repeat),
        );
        let interval = spin("Every", "", initial.interval, 99.0);
        let weekdays = adw::ActionRow::builder().title("On").build();
        let week_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        week_buttons.set_valign(gtk::Align::Center);
        let mut day_buttons = Vec::new();
        for day in WEEK {
            let button = gtk::ToggleButton::builder()
                .label(weekday_name(day))
                .active(initial.weekdays.contains(&day))
                .build();
            let state = state.clone();
            button.connect_toggled(move |button| {
                let mut draft = state.borrow_mut();
                draft.weekdays.retain(|chosen| *chosen != day);
                if button.is_active() {
                    draft.weekdays.push(day);
                }
                draft.rule_edited = true;
            });
            week_buttons.append(&button);
            day_buttons.push(button);
        }
        weekdays.add_suffix(&week_buttons);
        let ending = combo(
            "Ends",
            &["Never", "After a number of dates", "On a date"],
            ending_index(initial.ending),
        );
        let count = spin(
            "Number of dates",
            "",
            match initial.ending {
                Ending::After(count) => count,
                _ => 10,
            },
            999.0,
        );
        let until_day = match initial.ending {
            Ending::On(day) => day,
            _ => initial.start_date + chrono::Duration::days(90),
        };
        let until_picker = date_picker(until_day, {
            let state = state.clone();
            move |day| {
                let mut draft = state.borrow_mut();
                draft.ending = Ending::On(day);
                draft.rule_edited = true;
            }
        });
        let until_row = adw::ActionRow::builder().title("Last date").build();
        until_row.add_suffix(&until_picker.button);
        let kept = adw::ActionRow::builder()
            .title("Repeats by a rule set elsewhere")
            .subtitle("It is kept as it is unless you change the repeat here.")
            .build();
        for row in [
            &repeat.clone().upcast::<gtk::Widget>(),
            interval.upcast_ref(),
            weekdays.upcast_ref(),
            ending.upcast_ref(),
            count.upcast_ref(),
            until_row.upcast_ref(),
            kept.upcast_ref(),
        ] {
            repeat_group.add(row);
        }

        // What shows depends on the choices above.
        let sync: Rc<dyn Fn()> = {
            let state = state.clone();
            let (start_time_w, end_time_w) = (start_time.widget.clone(), end_time.widget.clone());
            let (end_row, has_end, until_row, count) = (
                end_row.clone(),
                has_end.clone(),
                until_row.clone(),
                count.clone(),
            );
            let (interval, weekdays, ending, kept) = (
                interval.clone(),
                weekdays.clone(),
                ending.clone(),
                kept.clone(),
            );
            Rc::new(move || {
                let draft = state.borrow();
                let repeating = draft.repeat != Repeat::Never;
                start_time_w.set_visible(!draft.all_day);
                end_time_w.set_visible(!draft.all_day);
                has_end.set_visible(!draft.all_day);
                end_row.set_visible(draft.all_day || draft.has_end);
                end_row.set_title(if draft.all_day { "Last day" } else { "Ends" });
                interval.set_visible(repeating);
                interval.set_subtitle(match draft.repeat {
                    Repeat::Never => "",
                    Repeat::Daily => "days",
                    Repeat::Weekly => "weeks",
                    Repeat::Monthly => "months",
                    Repeat::Yearly => "years",
                });
                weekdays.set_visible(draft.repeat == Repeat::Weekly);
                ending.set_visible(repeating);
                count.set_visible(repeating && matches!(draft.ending, Ending::After(_)));
                until_row.set_visible(repeating && matches!(draft.ending, Ending::On(_)));
                kept.set_visible(draft.rule_is_beyond_form() && !draft.rule_edited);
            })
        };
        sync();

        // Wire the fields to the draft.
        title.connect_changed({
            let state = state.clone();
            move |row| state.borrow_mut().title = row.text().to_string()
        });
        place.connect_changed({
            let state = state.clone();
            move |row| state.borrow_mut().location = row.text().to_string()
        });
        details.buffer().connect_changed({
            let state = state.clone();
            move |buffer| {
                let (start, end) = buffer.bounds();
                state.borrow_mut().details = buffer.text(&start, &end, false).to_string();
            }
        });
        all_day.connect_active_notify({
            let (state, sync) = (state.clone(), sync.clone());
            move |row| {
                state.borrow_mut().all_day = row.is_active();
                sync();
            }
        });
        has_end.connect_active_notify({
            let (state, sync) = (state.clone(), sync.clone());
            move |row| {
                state.borrow_mut().has_end = row.is_active();
                sync();
            }
        });
        repeat.connect_selected_notify({
            let (state, sync, day_buttons) = (state.clone(), sync.clone(), day_buttons);
            move |row| {
                let chosen = Repeat::ALL
                    .get(usize::try_from(row.selected()).unwrap_or(0))
                    .copied()
                    .unwrap_or(Repeat::Never);
                {
                    let mut draft = state.borrow_mut();
                    draft.repeat = chosen;
                    draft.rule_edited = true;
                    // A weekly series starts out on the weekday it starts on.
                    if chosen == Repeat::Weekly && draft.weekdays.is_empty() {
                        let weekday = weekday_of(draft.start_date);
                        draft.weekdays.push(weekday);
                    }
                }
                let chosen_days = state.borrow().weekdays.clone();
                for (button, day) in day_buttons.iter().zip(WEEK) {
                    button.set_active(chosen_days.contains(&day));
                }
                sync();
            }
        });
        interval.connect_value_notify({
            let state = state.clone();
            move |row| {
                let mut draft = state.borrow_mut();
                draft.interval = u32::try_from(row.value() as i64).unwrap_or(1).max(1);
                draft.rule_edited = true;
            }
        });
        ending.connect_selected_notify({
            let (state, sync, count, until_picker) = (
                state.clone(),
                sync.clone(),
                count.clone(),
                until_picker.set.clone(),
            );
            move |row| {
                {
                    let mut draft = state.borrow_mut();
                    draft.ending = match row.selected() {
                        1 => Ending::After(u32::try_from(count.value() as i64).unwrap_or(1).max(1)),
                        2 => Ending::On(draft.start_date + chrono::Duration::days(90)),
                        _ => Ending::Never,
                    };
                    draft.rule_edited = true;
                }
                if let Ending::On(day) = state.borrow().ending {
                    until_picker(day);
                }
                sync();
            }
        });
        count.connect_value_notify({
            let state = state.clone();
            move |row| {
                let mut draft = state.borrow_mut();
                if matches!(draft.ending, Ending::After(_)) {
                    draft.ending =
                        Ending::After(u32::try_from(row.value() as i64).unwrap_or(1).max(1));
                    draft.rule_edited = true;
                }
            }
        });

        // The page and its dialog.
        let page = adw::PreferencesPage::new();
        page.add(&what);
        page.add(&notes_group);
        page.add(&when);
        page.add(&repeat_group);
        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .margin_start(16)
            .margin_end(16)
            .margin_bottom(8)
            .build();
        error.add_css_class("error");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&page);
        body.append(&error);
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let save = gtk::Button::with_label(if editing { "Save" } else { "Create" });
        save.add_css_class("suggested-action");
        header.pack_end(&save);
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&body)
                .build(),
        ));
        let dialog = adw::Dialog::builder()
            .title(if editing { "Edit Event" } else { "New Event" })
            .content_width(520)
            .content_height(720)
            .child(&toolbar)
            .build();

        let window = self.clone();
        let generation = self.imp().generation.get();
        let (dialog_save, error_save, save_button) = (dialog.clone(), error.clone(), save.clone());
        save.connect_clicked(move |_| {
            let record = match state.borrow().to_event(&Local) {
                Ok(record) => record,
                Err(problem) => {
                    error_save.set_label(&problem.to_string());
                    error_save.set_visible(true);
                    return;
                }
            };
            error_save.set_visible(false);
            save_button.set_sensitive(false);
            let (window, session, dialog, error, button) = (
                window.clone(),
                session.clone(),
                dialog_save.clone(),
                error_save.clone(),
                save_button.clone(),
            );
            glib::spawn_future_local(async move {
                let saved = session.publish_event(record).await;
                if window.imp().generation.get() != generation {
                    return;
                }
                match saved {
                    Ok(event) => {
                        dialog.close();
                        window.event_saved(event);
                        window.toast(if editing {
                            "Event saved."
                        } else {
                            "Event created."
                        });
                    }
                    Err(message) => {
                        error.set_label(&message);
                        error.set_visible(true);
                        button.set_sensitive(true);
                    }
                }
            });
        });
        dialog.present(Some(self));
    }
}
