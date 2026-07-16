package social.waddle.android.feature.conversation

import social.waddle.android.client.auth.WaddleSessionInfo

/**
 * The origin whose cached XEP-0363 link-preview images may load: the
 * session's `link_preview_media_origin` when the server advertises
 * one, else the server base URL (web
 * `trustedLinkPreviewMediaOrigin` parity, which falls back to the
 * websocket origin — Android's server URL is that same origin).
 */
fun trustedPreviewOriginOf(session: WaddleSessionInfo?, serverUrl: String): String =
    session?.linkPreviewMediaOrigin?.takeIf { it.isNotBlank() } ?: serverUrl
