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
        bookmarks_loaded: false,
        muc_service: None,
        spaces_service: None,
        components: Vec::new(),
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
                has_left: channel
                    .user_bookmark
                    .is_some_and(|bookmark| bookmark.has_left()),
                bookmark_name: channel.bookmark_name,
                is_group_dm: channel.is_group_dm,
                members_only: channel.members_only,
                pin_permission: channel
                    .pin_permission
                    .and_then(crate::room_admin::pin_permission_to_ffi),
            })
            .collect(),
        bookmarks_loaded: topology.bookmarks_loaded,
        muc_service: topology.home_services.muc,
        spaces_service: topology.home_services.spaces,
        components: topology.home_services.components,
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

#[cfg(test)]
mod tests {
    use waddle_xmpp_client::discovery::{
        DiscoveredChannel, DiscoveredChannelType, DiscoveredHomeServices, DiscoveredTopology,
        SpaceNode,
    };
    use waddle_xmpp_client::xep::xep0045_owner::RoomPinPermission;

    use super::topology_to_ffi;
    use crate::WaddlePinPermission;

    #[test]
    fn discovered_home_service_jids_cross_the_ffi_even_without_catalog_entries() {
        let topology = topology_to_ffi(DiscoveredTopology {
            spaces: vec![],
            channels: vec![],
            bookmarks_loaded: false,
            home_services: DiscoveredHomeServices {
                muc: Some("rooms.example.net".parse().expect("MUC service JID")),
                spaces: None,
                components: vec!["files.example.net".parse().expect("component JID")],
            },
        });
        assert_eq!(
            topology.muc_service.as_ref().map(|jid| jid.as_str()),
            Some("rooms.example.net")
        );
        assert_eq!(topology.spaces_service, None);
        assert_eq!(topology.components[0].as_str(), "files.example.net");
    }

    fn channel(jid: &str, pin_permission: Option<RoomPinPermission>) -> DiscoveredChannel {
        DiscoveredChannel {
            id: jid.to_string(),
            room_jid: jid.parse().expect("room jid"),
            name: jid.to_string(),
            description: None,
            channel_type: DiscoveredChannelType::Text,
            position: 0,
            space_id: SpaceNode::try_new("standalone").expect("space node"),
            autojoin: true,
            user_bookmark: None,
            bookmark_name: None,
            is_group_dm: false,
            members_only: None,
            pin_permission,
        }
    }

    /// #2606: the room's pin policy from its disco#info reaches the apps.
    #[test]
    fn each_rooms_pin_permission_crosses_the_ffi() {
        let topology = topology_to_ffi(DiscoveredTopology {
            spaces: vec![],
            channels: vec![
                channel("open@muc.waddle.test", Some(RoomPinPermission::Anyone)),
                channel(
                    "strict@muc.waddle.test",
                    Some(RoomPinPermission::AdminsOnly),
                ),
                channel("unknown@muc.waddle.test", None),
            ],
            bookmarks_loaded: true,
            home_services: DiscoveredHomeServices::default(),
        });
        let pins: Vec<_> = topology
            .channels
            .iter()
            .map(|channel| channel.pin_permission)
            .collect();
        assert_eq!(
            pins,
            [
                Some(WaddlePinPermission::Anyone),
                Some(WaddlePinPermission::AdminsOnly),
                None,
            ]
        );
    }
}
