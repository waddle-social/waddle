mod owner;
mod owner_retire;
mod owner_update;
mod side_effects;
mod socket;
mod socket_forwarder;

pub(super) use owner::owner_remote_entry_if_current;
pub(super) use side_effects::{
    apply_remote_resource_presence_to_registry, apply_remote_resource_state,
};
