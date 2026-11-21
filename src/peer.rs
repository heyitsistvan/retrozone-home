//! A browser of the player's, connected straight to this computer.
//!
//! The site only passes the two sides' session descriptions between them (see `site.rs`). From
//! then on the browser and this program talk directly, over an encrypted WebRTC connection that
//! nobody else is part of: the game goes from this disk to that browser and nowhere else.
//!
//! The browser asks for each file over a data channel of its own, named after what it wants:
//!
//! - `{"want":"game","sha1":"…","title":"…","device":"…"}`: the file a game of the library is in,
//!   as it lies on the disk - the browser unpacks it and checks it is the game it asked for, as it
//!   does with a folder. The title and the kind of device are only for this computer's window to
//!   say what was played where;
//! - `{"want":"part","crc":"1A2B3C4D"}`: one file out of an arcade zip, by its CRC-32, for a set
//!   the browser puts together.
//!
//! The answer on that channel is a line of JSON - `{"size":…,"name":"…"}`, or `{"error":"…"}` -
//! followed by the bytes. The browser closes the channel once it has them all.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bytes::BytesMut;
use serde::Deserialize;
use tokio::sync::{mpsc, OwnedSemaphorePermit};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::peer_connection::{
    register_default_interceptors, MediaEngine, PeerConnection, PeerConnectionBuilder,
    PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceGatheringState, RTCIceServer,
    RTCPeerConnectionState, RTCSessionDescription, Registry,
};
use webrtc::runtime::{default_runtime, Runtime};

use crate::library::{Library, Want};

/// What is sent at once: well under the 256 KiB a browser takes in one message
const CHUNK: usize = 64 * 1024;
/// How much may be on its way to the browser before sending waits for it to catch up
const IN_FLIGHT: usize = 4 * 1024 * 1024;
/// How long gathering this computer's addresses may take before the answer goes without the rest. The
/// addresses that work come in at once; what is still out by then is an adapter that reaches nowhere
/// (a virtual machine's, a VPN's that is down), which would only keep the device waiting
const GATHERING: Duration = Duration::from_secs(2);
/// A connection is let go after this long, whatever the browser is doing
const LONGEST: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Deserialize)]
#[serde(tag = "want", rename_all = "lowercase")]
enum Asked {
    Game {
        sha1: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        device: String,
    },
    Part {
        crc: String,
    },
}

/// What this computer's window is told of
pub enum Report {
    /// A device connected, or let go of its connection
    Connected,
    Gone,
    /// A game was sent: its title as the device said it, the file it is, and the kind of device
    Sent { title: String, file: String, device: String },
}

pub type Reporter = Arc<dyn Fn(Report) + Send + Sync>;

struct Handler {
    runtime: Arc<dyn Runtime>,
    library: Arc<Library>,
    report: Reporter,
    gathered: mpsc::Sender<()>,
    over: mpsc::Sender<()>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        log::debug!("connection: {state}");
        // Not on "disconnected", which a connection can come back from: one that does not ends up failed
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            let _ = self.over.try_send(());
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let library = self.library.clone();
        let runtime = self.runtime.clone();
        let report = self.report.clone();
        let over = self.over.clone();
        self.runtime.spawn(Box::pin(async move {
            let label = channel.label().await.unwrap_or_default();
            let asked = match serde_json::from_str::<Asked>(&label) {
                Ok(asked) => asked,
                // The channel every connection opens first, to have something to offer. Nothing is asked on it,
                // and the browser closes it when it is done: the moment to let the connection go, rather than
                // when the connection is found to be dead some while later
                Err(_) => {
                    while let Some(event) = channel.poll().await {
                        if matches!(event, DataChannelEvent::OnClose) {
                            break;
                        }
                    }
                    let _ = over.try_send(());
                    return;
                }
            };

            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnOpen => {
                        let channel = channel.clone();
                        let library = library.clone();
                        let report = report.clone();
                        runtime.spawn(Box::pin(async move {
                            if let Err(error) = answer(&channel, &library, &report, asked).await {
                                log::warn!("sending stopped: {error:#}");
                            }
                        }));
                        // One question a channel
                        break;
                    }
                    DataChannelEvent::OnClose => return,
                    _ => {}
                }
            }
        }));
    }
}

/// Sends what was asked for on its channel
async fn answer(
    channel: &Arc<dyn DataChannel>,
    library: &Arc<Library>,
    report: &Reporter,
    asked: Asked,
) -> Result<()> {
    let (want, said) = match asked {
        Asked::Game { sha1, title, device } => (Want::Game(sha1.to_ascii_lowercase()), Some((title, device))),
        Asked::Part { crc } => (Want::Part(u32::from_str_radix(&crc, 16).unwrap_or(0)), None),
    };

    let lookup = library.clone();
    let wanted = want.clone();
    let found = tokio::task::spawn_blocking(move || lookup.read(&wanted)).await?;
    let found = match found {
        Ok(Some(found)) => found,
        // Not found while the folders are being read for the first time: it may well be among what is not read yet
        Ok(None) if library.is_looking() => {
            log::info!("asked for {want} while still looking through the folders");
            channel.send_text(r#"{"error":"looking"}"#).await?;
            return Ok(());
        }
        Ok(None) => {
            log::info!("asked for {want}, which is not here");
            channel.send_text(r#"{"error":"absent"}"#).await?;
            return Ok(());
        }
        Err(error) => {
            log::warn!("asked for {want}, which could not be read: {error:#}");
            channel.send_text(r#"{"error":"unreadable"}"#).await?;
            return Ok(());
        }
    };

    log::info!("sending {} ({} bytes)", found.name, found.bytes.len());
    let head = serde_json::json!({ "size": found.bytes.len(), "name": found.name });
    channel.send_text(&head.to_string()).await?;
    for piece in found.bytes.chunks(CHUNK) {
        channel.send(BytesMut::from(piece)).await?;
    }

    if let Some((title, device)) = said {
        let clip = |text: String, most: usize| text.chars().filter(|c| !c.is_control()).take(most).collect::<String>();
        report(Report::Sent { title: clip(title, 120), file: found.name, device: clip(device, 24) });
    }

    Ok(())
}

/// The STUN servers the site says to ask, as the browser was told them
pub fn ice_servers(urls: &[String]) -> Vec<RTCIceServer> {
    if urls.is_empty() {
        return Vec::new();
    }

    vec![RTCIceServer {
        urls: urls.to_vec(),
        ..Default::default()
    }]
}

/**
 * Takes a browser's offer and gives the answer that lets it connect. The connection then lives
 * on by itself, serving what the browser asks, until the browser goes or [`LONGEST`] is up, and
 * holds its `slot` among the connections there may be at once until then.
 */
pub async fn accept(
    offer: String,
    ice: Vec<RTCIceServer>,
    library: Arc<Library>,
    report: Reporter,
    slot: OwnedSemaphorePermit,
) -> Result<String> {
    let runtime = default_runtime().ok_or_else(|| anyhow!("no async runtime"))?;
    let (gathered_tx, mut gathered) = mpsc::channel(1);
    let (over_tx, mut over) = mpsc::channel(1);

    let mut media = MediaEngine::default();
    let registry = register_default_interceptors(Registry::new(), &mut media)?;
    let connection = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().with_ice_servers(ice).build())
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Handler {
            runtime: runtime.clone(),
            library,
            report: report.clone(),
            gathered: gathered_tx,
            over: over_tx,
        }))
        .with_runtime(runtime.clone())
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .with_data_channel_send_buffer_limit(IN_FLIGHT)
        .build()
        .await?;

    connection
        .set_remote_description(RTCSessionDescription::offer(offer)?)
        .await
        .context("the offer was not taken")?;
    let answer = connection.create_answer(None).await?;
    connection.set_local_description(answer).await?;
    // All of this computer's addresses in the one answer: there is no second message to send more in
    let _ = tokio::time::timeout(GATHERING, gathered.recv()).await;

    let sdp = connection
        .local_description()
        .await
        .ok_or_else(|| anyhow!("no answer was made"))?
        .sdp;

    // A browser that never connects lets its place go too
    report(Report::Connected);
    tokio::spawn(async move {
        let _ = tokio::time::timeout(LONGEST, over.recv()).await;
        let _ = connection.close().await;
        drop(slot);
        report(Report::Gone);
        log::debug!("connection let go");
    });

    Ok(sdp)
}
