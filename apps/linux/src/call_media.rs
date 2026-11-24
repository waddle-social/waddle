//! A call's audio: the LiveKit room the server's join credentials name, the
//! microphone and the speakers.
//!
//! libwebrtc's platform audio device module (through the LiveKit crate's
//! `PlatformAudio`) captures from the chosen microphone and plays remote
//! audio on the chosen speakers, over PulseAudio or ALSA, so this client
//! pushes and pulls no samples itself. The room runs on the tokio runtime;
//! its outcomes reach the GLib main loop as [`MediaEvent`]s, and the call
//! engine (`call_engine.rs`) decides what each one means for the call.

use livekit::prelude::{
    LocalAudioTrack, LocalTrack, PlatformAudio, PlayoutDeviceId, RecordingDeviceId, Room,
    RoomEvent, RoomOptions, TrackPublishOptions, TrackSource,
};
use livekit::webrtc::peer_connection_factory::IceServer;
use tokio::sync::mpsc;

use crate::call::{CallJoin, CallMedia};
use crate::runtime;

/// A microphone or speaker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Stable across hot-plug, unlike its position in the list.
    pub id: String,
    pub name: String,
}

/// What the room reports to the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaEvent {
    /// Joined the room and publishing the microphone.
    Connected {
        sid: String,
    },
    /// The room could not be joined or the devices could not be opened.
    Failed {
        sid: String,
    },
    /// The connection dropped; the SDK is trying to restore it.
    Reconnecting {
        sid: String,
    },
    Reconnected {
        sid: String,
    },
    /// The room ended after it was joined, and the SDK gave up on it.
    Lost {
        sid: String,
    },
    /// What this machine offers, and what is in use.
    Devices {
        sid: String,
        inputs: Vec<AudioDevice>,
        outputs: Vec<AudioDevice>,
    },
}

enum Command {
    Mute(bool),
    Input(String),
    Output(String),
    Close,
}

/// A XEP-0215 STUN or TURN service the server advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceService {
    pub kind: IceKind,
    pub host: String,
    pub port: Option<u16>,
    pub transport: Option<IceTransport>,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceKind {
    Stun,
    Turn,
    Turns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceTransport {
    Udp,
    Tcp,
}

/// The ICE servers WebRTC is given, from the advertised services.
///
/// STUN omits credentials and transport (RFC 7064). TURN needs both
/// credentials; only TURN carries a transport (RFC 7065). IPv6 hosts are
/// bracketed, and an omitted port uses the scheme default. A TURN service
/// without credentials is skipped: it could not be used.
pub fn ice_servers(services: &[IceService]) -> Vec<IceServer> {
    services
        .iter()
        .filter_map(|service| {
            let host = if service.host.contains(':') {
                format!("[{}]", service.host)
            } else {
                service.host.clone()
            };
            let host_port = match service.port {
                Some(port) => format!("{host}:{port}"),
                None => host,
            };
            if service.kind == IceKind::Stun {
                return Some(IceServer {
                    urls: vec![format!("stun:{host_port}")],
                    username: String::new(),
                    password: String::new(),
                });
            }
            let (username, password) = (service.username.clone()?, service.password.clone()?);
            let scheme = if service.kind == IceKind::Turn {
                "turn"
            } else {
                "turns"
            };
            let query = match service.transport {
                Some(IceTransport::Udp) => "?transport=udp",
                Some(IceTransport::Tcp) => "?transport=tcp",
                None => "",
            };
            Some(IceServer {
                urls: vec![format!("{scheme}:{host_port}{query}")],
                username,
                password,
            })
        })
        .collect()
}

/// A call's audio, from joining the room to leaving it. Dropping it leaves
/// the room and releases the audio devices.
pub struct MediaSession {
    commands: mpsc::UnboundedSender<Command>,
}

impl MediaSession {
    /// Join `join`'s room and publish the microphone, muted if `muted`.
    /// Outcomes arrive on `events`.
    pub fn start(
        sid: String,
        join: CallJoin,
        media: CallMedia,
        ice: Vec<IceServer>,
        muted: bool,
        events: async_channel::Sender<MediaEvent>,
    ) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        if muted {
            let _ = commands.send(Command::Mute(true));
        }
        runtime::spawn(run(sid, join, media, ice, events, receiver));
        Self { commands }
    }

    pub fn set_muted(&self, muted: bool) {
        let _ = self.commands.send(Command::Mute(muted));
    }

    pub fn select_input(&self, id: String) {
        let _ = self.commands.send(Command::Input(id));
    }

    pub fn select_output(&self, id: String) {
        let _ = self.commands.send(Command::Output(id));
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Close);
    }
}

fn devices(audio: &PlatformAudio) -> (Vec<AudioDevice>, Vec<AudioDevice>) {
    (
        audio
            .recording_devices()
            .map(|device| AudioDevice {
                id: device.id.as_str().to_string(),
                name: device.name,
            })
            .collect(),
        audio
            .playout_devices()
            .map(|device| AudioDevice {
                id: device.id.as_str().to_string(),
                name: device.name,
            })
            .collect(),
    )
}

/// What the room task is doing, for events that name the call.
struct Call {
    sid: String,
    events: async_channel::Sender<MediaEvent>,
}

impl Call {
    async fn tell(&self, make: impl FnOnce(String) -> MediaEvent) {
        // The window outlives the call; a send after it closed has nobody
        // to tell.
        let _ = self.events.send(make(self.sid.clone())).await;
    }
}

async fn run(
    sid: String,
    join: CallJoin,
    media: CallMedia,
    ice: Vec<IceServer>,
    events: async_channel::Sender<MediaEvent>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let call = Call { sid, events };
    if !media.audio {
        call.tell(|sid| MediaEvent::Failed { sid }).await;
        return;
    }
    // Held for the whole call: dropping the last handle releases the
    // devices.
    let audio = match PlatformAudio::new() {
        Ok(audio) => audio,
        Err(error) => {
            tracing::warn!(%error, "no audio device module");
            call.tell(|sid| MediaEvent::Failed { sid }).await;
            return;
        }
    };
    let mut options = RoomOptions::default();
    if !ice.is_empty() {
        options.rtc_config.ice_servers = ice;
    }
    let (room, mut room_events) = match Room::connect(&join.url, &join.token, options).await {
        Ok(joined) => joined,
        Err(error) => {
            tracing::warn!(%error, "could not join the call room");
            call.tell(|sid| MediaEvent::Failed { sid }).await;
            return;
        }
    };
    // The call may have ended while the room was being joined: leave
    // before the microphone opens.
    let mut muted = false;
    loop {
        match commands.try_recv() {
            Ok(Command::Mute(choice)) => muted = choice,
            Ok(Command::Close) | Err(mpsc::error::TryRecvError::Disconnected) => {
                let _ = room.close().await;
                return;
            }
            Ok(_) => {}
            Err(mpsc::error::TryRecvError::Empty) => break,
        }
    }
    let track = LocalAudioTrack::create_audio_track("microphone", audio.rtc_source());
    let publication = match room
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..TrackPublishOptions::default()
            },
        )
        .await
    {
        Ok(publication) => publication,
        Err(error) => {
            tracing::warn!(%error, "could not publish the microphone");
            let _ = room.close().await;
            call.tell(|sid| MediaEvent::Failed { sid }).await;
            return;
        }
    };
    if muted {
        publication.mute();
    }
    call.tell(|sid| MediaEvent::Connected { sid }).await;
    let (inputs, outputs) = devices(&audio);
    call.tell(|sid| MediaEvent::Devices {
        sid,
        inputs,
        outputs,
    })
    .await;

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Mute(true)) => publication.mute(),
                Some(Command::Mute(false)) => publication.unmute(),
                Some(Command::Input(id)) => {
                    if let Err(error) =
                        audio.switch_recording_device(&RecordingDeviceId::from_unchecked_guid(&id))
                    {
                        tracing::warn!(%error, "could not switch microphone");
                    }
                }
                Some(Command::Output(id)) => {
                    if let Err(error) =
                        audio.switch_playout_device(&PlayoutDeviceId::from_unchecked_guid(&id))
                    {
                        tracing::warn!(%error, "could not switch speakers");
                    }
                }
                // Closed or dropped: leave the room.
                Some(Command::Close) | None => {
                    let _ = room.close().await;
                    return;
                }
            },
            event = room_events.recv() => match event {
                Some(RoomEvent::Reconnecting) => {
                    call.tell(|sid| MediaEvent::Reconnecting { sid }).await;
                }
                Some(RoomEvent::Reconnected) => {
                    call.tell(|sid| MediaEvent::Reconnected { sid }).await;
                }
                Some(RoomEvent::Disconnected { reason }) => {
                    if reason != livekit::DisconnectReason::ClientInitiated {
                        call.tell(|sid| MediaEvent::Lost { sid }).await;
                    }
                    return;
                }
                None => {
                    call.tell(|sid| MediaEvent::Lost { sid }).await;
                    return;
                }
                Some(_) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(kind: IceKind, host: &str) -> IceService {
        IceService {
            kind,
            host: host.into(),
            port: None,
            transport: None,
            username: None,
            password: None,
        }
    }

    #[test]
    fn stun_carries_no_credentials_or_transport() {
        let servers = ice_servers(&[IceService {
            port: Some(3478),
            transport: Some(IceTransport::Udp),
            username: Some("ignored".into()),
            ..service(IceKind::Stun, "stun.example.org")
        }]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, vec!["stun:stun.example.org:3478"]);
        assert!(servers[0].username.is_empty() && servers[0].password.is_empty());
    }

    #[test]
    fn turn_needs_credentials_and_names_its_transport() {
        let with = |kind, transport| IceService {
            transport,
            username: Some("user".into()),
            password: Some("secret".into()),
            ..service(kind, "turn.example.org")
        };
        let servers = ice_servers(&[
            with(IceKind::Turn, Some(IceTransport::Udp)),
            with(IceKind::Turns, Some(IceTransport::Tcp)),
            with(IceKind::Turn, None),
            // No credentials: unusable, so skipped.
            service(IceKind::Turn, "bare.example.org"),
        ]);
        let urls: Vec<_> = servers
            .iter()
            .map(|server| server.urls[0].as_str())
            .collect();
        assert_eq!(
            urls,
            [
                "turn:turn.example.org?transport=udp",
                "turns:turn.example.org?transport=tcp",
                "turn:turn.example.org"
            ]
        );
        assert_eq!(servers[0].username, "user");
        assert_eq!(servers[0].password, "secret");
    }

    #[test]
    fn an_ipv6_host_is_bracketed() {
        let servers = ice_servers(&[IceService {
            port: Some(3478),
            ..service(IceKind::Stun, "2001:db8::1")
        }]);
        assert_eq!(servers[0].urls, vec!["stun:[2001:db8::1]:3478"]);
    }
}
