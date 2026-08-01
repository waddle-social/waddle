mod owner;
mod side_effects;
mod socket;

pub(super) use owner::owner_remote_entry_if_current;
pub(super) use side_effects::{
    apply_remote_resource_presence_to_registry, apply_remote_resource_state,
};
