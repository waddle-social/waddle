use super::*;

pub(super) async fn send_ws_text_frames<S, E, I>(
    sender: &mut S,
    frames: I,
    failure_message: &'static str,
) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
    I: IntoIterator<Item = String>,
{
    for frame in frames {
        debug!(len = frame.len(), "Sending XMPP WebSocket response");
        if !send_ws_message(sender, Message::Text(frame.into()), failure_message).await {
            return false;
        }
    }

    true
}

pub(super) async fn send_ws_message<S, E>(
    sender: &mut S,
    message: Message,
    failure_message: &'static str,
) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    match sender.send(message).await {
        Ok(()) => true,
        Err(error) => {
            error!(error = %error, "{failure_message}");
            false
        }
    }
}

pub(super) async fn close_ws_connection<S, E>(sender: &mut S, failure_message: &'static str) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    match sender.close().await {
        Ok(()) => true,
        Err(error) => {
            error!(error = %error, "{failure_message}");
            false
        }
    }
}
