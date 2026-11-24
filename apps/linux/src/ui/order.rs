//! Message order: Chat (newest at the bottom) or Social (newest first).
//!
//! The row store always holds messages oldest first, in step with the
//! core's timeline, so every diff and index in the window stays valid. In
//! Social order the `ListView` reads that store through [`ReverseModel`],
//! the composer moves above the timeline, and the scroll helpers swap their
//! ends: "newest" is the top and older history loads at the bottom.

use std::cell::RefCell;

use adw::prelude::{AdjustmentExt, Cast, CastNone, ListModelExt, ObjectExt, StaticType, WidgetExt};
use adw::subclass::prelude::{ListModelImpl, ObjectImpl, ObjectSubclass, ObjectSubclassIsExt};
use gtk::{gio, glib};

use super::WaddleWindow;
use crate::settings::{self, MessageOrder};

mod imp {
    #[cfg_attr(
        not(test),
        expect(
            clippy::wildcard_imports,
            reason = "inline subclass parent imports are an explicit Rust style exception"
        )
    )]
    use super::*;

    #[derive(Default)]
    pub struct ReverseModel {
        pub source: RefCell<Option<gio::ListModel>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ReverseModel {
        const NAME: &'static str = "WaddleReverseModel";
        type Type = super::ReverseModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for ReverseModel {}

    impl ListModelImpl for ReverseModel {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.source
                .borrow()
                .as_ref()
                .map_or(0, |source| source.n_items())
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let source = self.source.borrow();
            let source = source.as_ref()?;
            source.item(mirror(source.n_items(), position)?)
        }
    }
}

glib::wrapper! {
    /// A list model showing another one back to front.
    pub struct ReverseModel(ObjectSubclass<imp::ReverseModel>)
        @implements gio::ListModel;
}

impl ReverseModel {
    pub fn new(source: &gio::ListModel) -> Self {
        let model: Self = glib::Object::new();
        model.imp().source.replace(Some(source.clone()));
        let weak = model.downgrade();
        source.connect_items_changed(move |_, position, removed, added| {
            if let Some(model) = weak.upgrade() {
                let (start, removed, added) =
                    reversed_change(model.n_items(), position, removed, added);
                model.items_changed(start, removed, added);
            }
        });
        model
    }
}

/// The position in a list of `len` items that mirrors `position`.
fn mirror(len: u32, position: u32) -> Option<u32> {
    len.checked_sub(position)?.checked_sub(1)
}

/// A change to a source (`position`, `removed`, `added`) as the reversed
/// model sees it, given the source's length after the change.
fn reversed_change(new_len: u32, position: u32, removed: u32, added: u32) -> (u32, u32, u32) {
    (
        new_len.saturating_sub(position).saturating_sub(added),
        removed,
        added,
    )
}

/// The `ListView` position of a store index.
pub(super) fn display_index(order: MessageOrder, len: usize, index: usize) -> usize {
    match order {
        MessageOrder::Chat => index,
        MessageOrder::Social => len.saturating_sub(index + 1),
    }
}

impl WaddleWindow {
    /// The order in force.
    pub(super) fn message_order(&self) -> MessageOrder {
        settings::get().message_order
    }

    /// Point the timeline and composer at the chosen order. Also called once
    /// when the timeline is set up.
    pub(super) fn apply_message_order(&self) {
        let imp = self.imp();
        let Some(rows) = imp.rows.borrow().clone() else {
            return;
        };
        let order = self.message_order();
        let model: gio::ListModel = match order {
            MessageOrder::Chat => rows.upcast(),
            MessageOrder::Social => ReverseModel::new(rows.upcast_ref()).upcast(),
        };
        imp.timeline_view
            .set_model(Some(&gtk::NoSelection::new(Some(model))));
        self.place_composer(order);
        // Rows are laid out for the order they were bound in.
        self.rebuild_rows();
        self.scroll_to_end();
    }

    /// The composer is the last bar of the timeline's toolbar view: at the
    /// bottom in Chat order, and directly above the timeline in Social.
    fn place_composer(&self, order: MessageOrder) {
        let imp = self.imp();
        let Some(bar) = imp.composer.parent() else {
            return;
        };
        let Some(toolbar) = bar
            .ancestor(adw::ToolbarView::static_type())
            .and_downcast::<adw::ToolbarView>()
        else {
            return;
        };
        let on_top = order == MessageOrder::Social;
        bar.set_margin_top(if on_top { 8 } else { 0 });
        bar.set_margin_bottom(if on_top { 8 } else { 16 });
        toolbar.remove(&bar);
        if on_top {
            toolbar.add_top_bar(&bar);
        } else {
            toolbar.add_bottom_bar(&bar);
        }
    }

    /// Whether the view shows the newest message edge, close enough to
    /// follow new arrivals.
    pub(super) fn near_newest(&self) -> bool {
        let adjustment = self.imp().timeline_scroller.vadjustment();
        match self.message_order() {
            MessageOrder::Chat => {
                adjustment.upper() - adjustment.page_size() - adjustment.value() < 120.0
            }
            MessageOrder::Social => adjustment.value() < 120.0,
        }
    }

    /// Whether the view is near the oldest loaded message, where earlier
    /// history should page in.
    pub(super) fn near_oldest(&self) -> bool {
        let adjustment = self.imp().timeline_scroller.vadjustment();
        match self.message_order() {
            MessageOrder::Chat => adjustment.value() < 200.0,
            MessageOrder::Social => {
                adjustment.upper() - adjustment.page_size() - adjustment.value() < 200.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_maps_ends_and_rejects_out_of_range() {
        assert_eq!(mirror(3, 0), Some(2));
        assert_eq!(mirror(3, 2), Some(0));
        assert_eq!(mirror(3, 3), None);
        assert_eq!(mirror(0, 0), None);
    }

    #[test]
    fn changes_are_mirrored() {
        // Append one to two: the new newest message is at the top.
        assert_eq!(reversed_change(3, 2, 0, 1), (0, 0, 1));
        // Older history pushed to the front of the store lands at the bottom.
        assert_eq!(reversed_change(5, 0, 0, 3), (2, 0, 3));
        // Replace the oldest of three: it is the last row.
        assert_eq!(reversed_change(3, 0, 1, 1), (2, 1, 1));
        // Clear a list of three.
        assert_eq!(reversed_change(0, 0, 3, 0), (0, 3, 0));
    }

    #[test]
    fn display_index_follows_the_order() {
        assert_eq!(display_index(MessageOrder::Chat, 4, 1), 1);
        assert_eq!(display_index(MessageOrder::Social, 4, 0), 3);
        assert_eq!(display_index(MessageOrder::Social, 4, 3), 0);
    }

    #[test]
    fn the_model_reads_its_source_back_to_front() {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let reversed = ReverseModel::new(store.upcast_ref());
        let changes = std::rc::Rc::new(RefCell::new(Vec::new()));
        let seen = changes.clone();
        reversed.connect_items_changed(move |_, position, removed, added| {
            seen.borrow_mut().push((position, removed, added));
        });
        let items: Vec<_> = (0..3_u32).map(glib::BoxedAnyObject::new).collect();
        store.extend_from_slice(&items);
        let value = |position| {
            reversed
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
                .map(|object| *object.borrow::<u32>())
        };
        assert_eq!(reversed.n_items(), 3);
        assert_eq!(
            (value(0), value(1), value(2), value(3)),
            (Some(2), Some(1), Some(0), None)
        );
        // An older message pushed to the front appears at the end.
        store.splice(0, 0, &[glib::BoxedAnyObject::new(9_u32)]);
        assert_eq!(value(3), Some(9));
        assert_eq!(value(0), Some(2));
        assert_eq!(*changes.borrow(), [(0, 0, 3), (3, 0, 1)]);
    }
}
