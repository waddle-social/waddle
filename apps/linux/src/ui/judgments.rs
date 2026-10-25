//! Jev message badges and their inline probability breakdown.

use adw::prelude::{BoxExt, WidgetExt};
use waddle_client_session::judgments::SafetyCategory;
use waddle_client_session::TimelineItem;

/// Rebinding rebuilds the disclosure so recycled rows never retain another
/// message's expanded judgments. Labels contain plain text, never markup.
pub(super) fn bind(container: &gtk::Box, item: &TimelineItem) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    container.set_visible(false);
    let Some(scores) = item.scores.as_ref().filter(|_| !item.retracted) else {
        return;
    };
    let notable = scores.notable();
    if notable.is_empty() {
        return;
    }
    let disclosure = gtk::Expander::new(None);
    let badges = gtk::Box::builder().spacing(8).build();
    if let Some(leading) = scores.leading() {
        let label = gtk::Label::new(Some(leading.category.label()));
        label.add_css_class(if leading.probability.value() >= 0.8 {
            "error"
        } else {
            "warning"
        });
        badges.append(&label);
    }
    if notable
        .iter()
        .any(|score| score.category == SafetyCategory::IsQuestion)
    {
        let label = gtk::Label::new(Some("Question"));
        label.add_css_class("accent");
        badges.append(&label);
    }
    disclosure.set_label_widget(Some(&badges));
    disclosure.set_tooltip_text(Some("Show Jev judgments"));
    let breakdown = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(6)
        .build();
    for score in notable {
        let row = gtk::Box::builder().spacing(8).build();
        let label = gtk::Label::builder()
            .label(score.category.label())
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        label.set_tooltip_text(Some(&score.taxonomy_version));
        let percent = gtk::Label::new(Some(&format!("{:.0}%", score.probability.value() * 100.0)));
        percent.add_css_class("numeric");
        row.append(&label);
        row.append(&percent);
        breakdown.append(&row);
    }
    let model = gtk::Label::builder()
        .label(format!("Model {}", scores.model_version))
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    model.add_css_class("dim-label");
    model.add_css_class("caption");
    breakdown.append(&model);
    disclosure.set_child(Some(&breakdown));
    container.append(&disclosure);
    container.set_visible(true);
}
