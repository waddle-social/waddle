# Frozen invitation connection route

This is a behavior-preserving package B slice of [#2046](https://github.com/waddle-social/waddle/issues/2046), stacked on the room route decision PR. An independent architecture review cleared only this dedicated invitation operation.

## Plan

1. Move the current local then registered-remote `DirectFrame` selection from `ingress/execute_invite.rs` into `interpret::route_to_connection::deliver_frozen_invitation_resource`.
2. Keep the frozen resource list, canonical invitation receipt, UOW/context setup, and pending fallback settlement at the caller. Remote selection happens only after definite local `Unavailable`; `MaybeCommitted` remains uncertain and suppresses another attempt or fallback.
3. Extend focused invitation tests for local delivery, remote delivery, remote uncertain completion, and definitive unavailability. Run Rust formatting, strict Clippy, and the relevant native tests.

This is a full-JID alias route over existing implementations, not exact `ConnectionRef` endpoint authority, a new custody lane, or a wire-format change. Do not reuse generic direct delivery, which includes detached and other paths outside invitation semantics.
