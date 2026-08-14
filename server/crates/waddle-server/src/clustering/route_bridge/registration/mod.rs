mod owner;
mod owner_retire;
mod owner_update;
mod side_effects;
pub(crate) use side_effects::RemoteCarbonFanout;
mod socket;
mod socket_forwarder;

pub(super) use owner::owner_remote_entry_if_current;
pub(super) use side_effects::{
    apply_remote_resource_presence_to_registry, apply_remote_resource_state,
};
#[cfg(test)]
pub(crate) use socket::retry_remote_resource_register_test;
