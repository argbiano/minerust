use super::{Bedrock, identity, session};
use anyhow::{Context, Result, bail};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use tracing::{debug, info};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceCandidateType, RTCIceGatheringState, RTCPeerConnectionState, RTCSessionDescription, SettingEngineBuilder,
    },
};

pub const RELIABLE: &str = "ReliableDataChannel";
pub const UNRELIABLE: &str = "UnreliableDataChannel";

const GATHER_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A message from either data channel, header byte included.
pub struct Incoming {
    pub reliable: bool,
    pub data: Vec<u8>,
}

struct Events {
    gathered: mpsc::Sender<()>,
    channels: mpsc::UnboundedSender<Arc<dyn DataChannel>>,
    closed: mpsc::Sender<()>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed | RTCPeerConnectionState::Disconnected
        ) {
            let _ = self.closed.try_send(());
        }
    }

    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        let _ = self.channels.send(dc);
    }
}

pub async fn accept(bedrock: Arc<Bedrock>, network_id: String, offer: String) -> Result<String> {
    if identity::has_identity(&offer) {
        // Checking the player's token needs the Minecraft auth service keys; offline for now.
        debug!("{network_id}: ignoring the offer's identity assertion");
    }

    let (gathered_tx, mut gathered) = mpsc::channel(1);
    let (channels_tx, channels) = mpsc::unbounded_channel();
    let (closed_tx, closed) = mpsc::channel(1);
    let events = Events { gathered: gathered_tx, channels: channels_tx, closed: closed_tx };

    let config = &bedrock.server.config;
    let mut settings = SettingEngineBuilder::new();
    if let Some(ip) = config.bedrock_public_ip {
        settings = settings.with_nat_1to1_ips(vec![ip.to_string()], RTCIceCandidateType::Host);
    }
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_setting_engine(settings.build())
        .with_handler(Arc::new(events))
        .with_runtime(bedrock.runtime.clone())
        .with_udp_addrs(vec![config.bedrock_udp_bind.clone()])
        .build()
        .await?;

    let answer = async {
        pc.set_remote_description(RTCSessionDescription::offer(identity::strip_identity(&offer))?).await?;
        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer).await?;
        timeout(GATHER_TIMEOUT, gathered.recv()).await.context("ICE gathering timed out")?;
        let local = pc.local_description().await.context("no local description")?;
        bedrock.key.sign_answer(&local.sdp)
    }
    .await;

    match answer {
        Ok(answer) => {
            tokio::spawn(serve(bedrock, network_id, pc, channels, closed));
            Ok(answer)
        }
        Err(e) => {
            let _ = pc.close().await;
            Err(e)
        }
    }
}

async fn serve(
    bedrock: Arc<Bedrock>,
    network_id: String,
    pc: impl PeerConnection,
    mut channels: mpsc::UnboundedReceiver<Arc<dyn DataChannel>>,
    mut closed: mpsc::Receiver<()>,
) {
    bedrock.server.bedrock_peers.fetch_add(1, Ordering::Relaxed);
    let result = async {
        let (tx, rx) = mpsc::channel(256);
        let mut reliable = None;
        let wait = async {
            while let Some(dc) = channels.recv().await {
                match dc.label().await?.as_str() {
                    RELIABLE => {
                        spawn_reader(dc.clone(), true, tx.clone());
                        reliable = Some(dc);
                    }
                    UNRELIABLE => spawn_reader(dc, false, tx.clone()),
                    other => debug!("{network_id}: ignoring data channel {other:?}"),
                }
                if reliable.is_some() {
                    return Ok(());
                }
            }
            bail!("peer connection went away")
        };
        tokio::select! {
            r = timeout(CONNECT_TIMEOUT, wait) => r.context("no ReliableDataChannel")??,
            _ = closed.recv() => bail!("connection failed before the data channels opened"),
        }
        let reliable = reliable.unwrap();
        wait_open(&reliable).await?;
        info!("{network_id}: NetherNet connection established");

        // Late unreliable channels still get read so they never back up the association.
        let tx_late = tx.clone();
        tokio::spawn(async move {
            while let Some(dc) = channels.recv().await {
                if dc.label().await.is_ok_and(|l| l == UNRELIABLE) {
                    spawn_reader(dc, false, tx_late.clone());
                }
            }
        });
        drop(tx);

        tokio::select! {
            r = session::run(&bedrock, &network_id, reliable.clone(), rx) => r,
            _ = closed.recv() => bail!("peer connection closed"),
        }
    }
    .await;

    match result {
        Ok(()) => info!("{network_id}: disconnected"),
        Err(e) => info!("{network_id}: disconnected: {e:#}"),
    }
    // Give the last packets a moment to leave before tearing down DTLS.
    sleep(Duration::from_millis(250)).await;
    let _ = pc.close().await;
    bedrock.server.bedrock_peers.fetch_sub(1, Ordering::Relaxed);
}

async fn wait_open(dc: &Arc<dyn DataChannel>) -> Result<()> {
    for _ in 0..200 {
        if dc.ready_state().await? == webrtc::data_channel::RTCDataChannelState::Open {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    bail!("ReliableDataChannel never opened")
}

fn spawn_reader(dc: Arc<dyn DataChannel>, reliable: bool, tx: mpsc::Sender<Incoming>) {
    tokio::spawn(async move {
        while let Some(event) = dc.poll().await {
            match event {
                DataChannelEvent::OnMessage(msg) => {
                    if tx.send(Incoming { reliable, data: msg.data.to_vec() }).await.is_err() {
                        break;
                    }
                }
                DataChannelEvent::OnClose => break,
                _ => {}
            }
        }
    });
}
