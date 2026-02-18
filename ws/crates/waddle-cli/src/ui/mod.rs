// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! UI module for the Waddle TUI.
//!
//! This module contains all the UI components and rendering logic:
//! - `layout` - Main layout with three panels
//! - `sidebar` - Sidebar widget for waddles/channels/DMs
//! - `messages` - Message view widget
//! - `input` - Input area widget

pub mod input;
pub mod layout;
pub mod messages;
pub mod sidebar;

pub use input::InputWidget;
pub use layout::render_layout;
pub use messages::MessagesWidget;
pub use sidebar::SidebarWidget;
