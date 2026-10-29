//! Loopback-only peer for check-libp2p-rollout.py; built in isolated workspaces.
use std::{
    error::Error,
    io::{self, Write},
    time::Duration,
};

use futures::StreamExt;
use kameo::{
    actor::{RemoteActorRef, Spawn},
    message::{Context, Message},
    Actor, RemoteActor,
};
use libp2p::{noise, swarm::SwarmEvent, tcp, yamux, SwarmBuilder};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

type ProbeResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Actor, RemoteActor)]
#[remote_actor(id = "waddle.rollout.echo-actor.v1")]
struct EchoActor {
    label: String,
}

#[derive(Serialize, Deserialize)]
struct Echo {
    nonce: String,
}

#[derive(Serialize, Deserialize, kameo::Reply)]
struct EchoReply {
    label: String,
    nonce: String,
}

#[kameo::remote_message("waddle.rollout.echo.v1")]
impl Message<Echo> for EchoActor {
    type Reply = EchoReply;

    async fn handle(&mut self, message: Echo, _: &mut Context<Self, Self::Reply>) -> EchoReply {
        EchoReply {
            label: self.label.clone(),
            nonce: message.nonce,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Peer {
    peer_id: String,
    address: String,
    label: String,
}

fn output(value: &impl Serialize) -> ProbeResult<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ProbeResult<()> {
    tokio::time::timeout(Duration::from_secs(45), run()).await??;
    Ok(())
}

async fn run() -> ProbeResult<()> {
    let arguments: Vec<_> = std::env::args().collect();
    let label = arguments.get(1).ok_or("missing peer label")?.clone();
    let transport = arguments.get(2).ok_or("missing transport")?;
    let dialer = arguments.get(3).ok_or("missing dialer flag")? == "dial";
    let mut swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            kameo::remote::Behaviour::new(
                key.public().to_peer_id(),
                kameo::remote::messaging::Config::default(),
            )
        })?
        .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_secs(40)))
        .build();
    swarm.behaviour().try_init_global()?;
    let actor = EchoActor::spawn(EchoActor {
        label: label.clone(),
    });
    let listen = match transport.as_str() {
        "tcp" => "/ip4/127.0.0.1/tcp/0",
        "quic" => "/ip4/127.0.0.1/udp/0/quic-v1",
        _ => return Err("unknown transport".into()),
    };
    swarm.listen_on(listen.parse()?)?;
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break address;
        }
    };
    output(&Peer {
        peer_id: swarm.local_peer_id().to_string(),
        address: address.to_string(),
        label: label.clone(),
    })?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let peer: Peer = serde_json::from_str(&input)?;
    let remote_peer = peer.peer_id.parse()?;
    let remote_address = peer.address.parse()?;
    swarm.add_peer_address(remote_peer, remote_address);
    if dialer {
        swarm.dial(
            libp2p::swarm::dial_opts::DialOpts::peer_id(remote_peer)
                .addresses(vec![peer.address.parse()?])
                .build(),
        )?;
    }
    let (stop_tx, mut stop_rx) = oneshot::channel();
    std::thread::spawn(move || {
        let mut stop = String::new();
        let result = io::stdin().read_line(&mut stop);
        let _ = stop_tx.send(result.map(|_| stop));
    });
    let (result_tx, mut result_rx) = oneshot::channel();
    let mut worker = Some((actor.clone(), result_tx));
    let mut complete = false;
    loop {
        tokio::select! {
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == remote_peer => {
                        if let Some((actor, result_tx)) = worker.take() {
                            let label = label.clone();
                            let remote_label = peer.label.clone();
                            tokio::spawn(async move {
                                let result: ProbeResult<()> = async {
                                    actor.register(format!("rollout-{label}")).await?;
                                    let remote = loop {
                                        if let Some(remote) = RemoteActorRef::<EchoActor>::lookup(format!("rollout-{remote_label}")).await? {
                                            break remote;
                                        }
                                        tokio::time::sleep(Duration::from_millis(50)).await;
                                    };
                                    let nonce = format!("{label}-to-{remote_label}");
                                    let reply = remote.ask(&Echo { nonce: nonce.clone() }).await?;
                                    if reply.label != remote_label || reply.nonce != nonce {
                                        return Err("remote actor returned the wrong peer or payload".into());
                                    }
                                    Ok(())
                                }.await;
                                let _ = result_tx.send(result);
                            });
                        }
                    },
                    SwarmEvent::OutgoingConnectionError { error, .. } => return Err(error.into()),
                    _ => {},
                }
            },
            result = &mut result_rx, if !complete => {
                result??;
                output(&serde_json::json!({"ok": true, "peer": label, "transport": transport, "registry_lookup": true, "remote_ask": true}))?;
                complete = true;
            },
            stop = &mut stop_rx => {
                let stop = stop??;
                if stop.trim() != "stop" || !complete {
                    return Err("peer stopped before compatibility checks completed".into());
                }
                actor.stop_gracefully().await?;
                return Ok(());
            },
        }
    }
}
