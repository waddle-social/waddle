// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! HTTP API client for Waddle CLI.
//!
//! Provides functions to fetch real data from the waddle-server HTTP API.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Response for a single waddle from the API
#[derive(Debug, Clone, Deserialize)]
pub struct WaddleResponse {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_user_id: String,
    pub icon_url: Option<String>,
    pub is_public: bool,
    pub role: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
}

/// Response for list of waddles
#[derive(Debug, Deserialize)]
pub struct ListWaddlesResponse {
    pub waddles: Vec<WaddleResponse>,
    pub total: usize,
}

/// Response for a single channel from the API
#[derive(Debug, Clone, Deserialize)]
pub struct ChannelResponse {
    pub id: String,
    pub waddle_id: String,
    pub name: String,
    pub description: Option<String>,
    pub channel_type: String,
    pub position: i32,
    pub is_default: bool,
    pub created_at: String,
    pub updated_at: Option<String>,
}

/// Response for list of channels
#[derive(Debug, Deserialize)]
pub struct ListChannelsResponse {
    pub channels: Vec<ChannelResponse>,
    pub total: usize,
}

/// Request to create a new waddle
#[derive(Debug, Serialize)]
pub struct CreateWaddleRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    pub is_public: bool,
}

impl CreateWaddleRequest {
    /// Create a new waddle request with just a name (public by default)
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            icon_url: None,
            is_public: true,
        }
    }

    /// Set the description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set whether the waddle is public
    pub fn with_public(mut self, is_public: bool) -> Self {
        self.is_public = is_public;
        self
    }
}

/// API client for communicating with waddle-server
pub struct ApiClient {
    client: reqwest::Client,
    base_url: String,
    session_token: String,
}

impl ApiClient {
    /// Create a new API client
    pub fn new(base_url: &str, session_token: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            session_token: session_token.to_string(),
        }
    }

    /// Fetch all waddles the user has access to
    pub async fn list_waddles(&self) -> Result<Vec<WaddleResponse>> {
        let url = format!(
            "{}/v1/waddles?session_id={}",
            self.base_url, self.session_token
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to server")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Failed to fetch waddles: {} - {}",
                status,
                text
            ));
        }

        let list: ListWaddlesResponse = response
            .json()
            .await
            .context("Failed to parse waddles response")?;

        Ok(list.waddles)
    }

    /// Fetch all channels for a specific waddle
    pub async fn list_channels(&self, waddle_id: &str) -> Result<Vec<ChannelResponse>> {
        let url = format!(
            "{}/v1/waddles/{}/channels?session_id={}",
            self.base_url, waddle_id, self.session_token
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to server")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Failed to fetch channels: {} - {}",
                status,
                text
            ));
        }

        let list: ListChannelsResponse = response
            .json()
            .await
            .context("Failed to parse channels response")?;

        Ok(list.channels)
    }

    /// Fetch all waddles and their channels
    pub async fn fetch_all(&self) -> Result<(Vec<WaddleResponse>, Vec<ChannelResponse>)> {
        // First fetch all waddles
        let waddles = self.list_waddles().await?;

        // Then fetch channels for each waddle
        let mut all_channels = Vec::new();
        for waddle in &waddles {
            match self.list_channels(&waddle.id).await {
                Ok(channels) => all_channels.extend(channels),
                Err(e) => {
                    tracing::warn!("Failed to fetch channels for waddle {}: {}", waddle.id, e);
                }
            }
        }

        Ok((waddles, all_channels))
    }

    /// Create a new waddle
    pub async fn create_waddle(&self, request: CreateWaddleRequest) -> Result<WaddleResponse> {
        let url = format!(
            "{}/v1/waddles?session_id={}",
            self.base_url, self.session_token
        );

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to connect to server")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Failed to create waddle: {} - {}",
                status,
                text
            ));
        }

        let waddle: WaddleResponse = response
            .json()
            .await
            .context("Failed to parse waddle response")?;

        Ok(waddle)
    }
}
