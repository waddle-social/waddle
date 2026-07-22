use crate::{
    WaddleChannel, WaddleMamPage, WaddleSpace, WaddleTopology, WaddleUploadHeader, WaddleUploadSlot,
};

/// Extract the domain part from a JID like `user@domain` or `domain`.
pub(crate) fn jid_domain(jid: &str) -> &str {
    jid.split('@').next_back().unwrap_or(jid)
}

pub(crate) fn empty_topology() -> WaddleTopology {
    WaddleTopology {
        spaces: Vec::new(),
        channels: Vec::new(),
    }
}

pub(crate) fn topology_to_ffi(
    topology: waddle_xmpp_client::discovery::DiscoveredTopology,
) -> WaddleTopology {
    WaddleTopology {
        spaces: topology
            .spaces
            .into_iter()
            .map(|space| WaddleSpace {
                id: space.id.as_str().to_string(),
                service_jid: space.service_jid.to_string(),
                name: space.name,
                description: space.description,
            })
            .collect(),
        channels: topology
            .channels
            .into_iter()
            .map(|channel| WaddleChannel {
                id: channel.id,
                room_jid: channel.room_jid.to_string(),
                name: channel.name,
                description: channel.description,
                channel_type: channel.channel_type.as_str().to_string(),
                position: channel.position,
                space_id: channel.space_id.as_str().to_string(),
                autojoin: channel.autojoin,
                bookmark_name: channel.bookmark_name,
                is_group_dm: channel.is_group_dm,
            })
            .collect(),
    }
}

pub(crate) fn empty_mam_page() -> WaddleMamPage {
    WaddleMamPage {
        messages: vec![],
        first_id: None,
        last_id: None,
        is_complete: false,
    }
}

pub(crate) fn mam_page_to_ffi(page: waddle_xmpp_client::mam::MamPage) -> WaddleMamPage {
    WaddleMamPage {
        // filter_map: rows whose trusted parse was rejected and that carry
        // no call event are dropped (spoofed-moderation guard, wasm parity).
        messages: page
            .messages
            .into_iter()
            .filter_map(crate::convert::archived_to_ffi)
            .collect(),
        first_id: page.rsm.first,
        last_id: page.rsm.last,
        is_complete: page.is_complete,
    }
}

pub(crate) fn upload_slot_to_ffi(
    slot: waddle_xmpp_client::discovery::UploadSlot,
) -> WaddleUploadSlot {
    WaddleUploadSlot {
        put_url: slot.put_url,
        get_url: slot.get_url,
        put_headers: slot
            .put_headers
            .into_iter()
            .map(|(name, value)| WaddleUploadHeader { name, value })
            .collect(),
    }
}
