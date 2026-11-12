use super::{
    Bedrock, PROTOCOL_VERSION,
    batch::{self, Framing, Packet, put_string, put_varint},
    framing::{self, Reassembler},
    peer::Incoming,
};
use anyhow::{Context, Result, bail, ensure};
use bytes::BytesMut;
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, time::timeout};
use tracing::debug;
use webrtc::data_channel::DataChannel;

const LOGIN: u32 = 1;
const PLAY_STATUS: u32 = 2;
const DISCONNECT: u32 = 5;
const NETWORK_SETTINGS: u32 = 143;
const REQUEST_NETWORK_SETTINGS: u32 = 193;

const LOGIN_FAILED_CLIENT: i32 = 1;
const LOGIN_FAILED_SERVER: i32 = 2;

const COMPRESSION_THRESHOLD: u16 = 256;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

struct Session {
    dc: Arc<dyn DataChannel>,
    framing: Framing,
}

impl Session {
    async fn send(&self, packets: &[Packet]) -> Result<()> {
        let data = batch::encode(packets, self.framing)?;
        for fragment in framing::split(&data)? {
            self.dc.send(BytesMut::from(&fragment[..])).await?;
        }
        Ok(())
    }

    async fn disconnect(&self, message: &str) -> Result<()> {
        let mut body = Vec::new();
        put_varint(&mut body, 0); // reason: unknown, the message says it all
        body.push(0); // message follows
        put_string(&mut body, message);
        put_string(&mut body, message);
        self.send(&[Packet::new(DISCONNECT, body)]).await
    }
}

pub async fn run(
    _bedrock: &Bedrock,
    peer: &str,
    dc: Arc<dyn DataChannel>,
    mut rx: mpsc::Receiver<Incoming>,
) -> Result<()> {
    let mut session = Session { dc, framing: Framing::Raw };
    let mut reliable = Reassembler::default();

    loop {
        let msg =
            timeout(IDLE_TIMEOUT, rx.recv()).await.context("client went quiet")?.context("data channel closed")?;
        let data = if msg.reliable {
            match reliable.push(&msg.data)? {
                Some(data) => data,
                None => continue,
            }
        } else {
            ensure!(msg.data.first() == Some(&0), "fragmented message on the unreliable channel");
            msg.data[1..].to_vec()
        };

        for packet in batch::decode(&data, session.framing)? {
            match packet.id {
                REQUEST_NETWORK_SETTINGS => {
                    let version = packet.body.get(..4).context("short RequestNetworkSettings")?;
                    let version = i32::from_be_bytes(version.try_into()?);
                    if version != PROTOCOL_VERSION {
                        let status = if version < PROTOCOL_VERSION { LOGIN_FAILED_CLIENT } else { LOGIN_FAILED_SERVER };
                        session.send(&[Packet::new(PLAY_STATUS, status.to_be_bytes().to_vec())]).await?;
                        bail!("client speaks protocol {version}, we speak {PROTOCOL_VERSION}");
                    }
                    session.send(&[network_settings()]).await?;
                    session.framing = Framing::Deflate { threshold: COMPRESSION_THRESHOLD as usize };
                }
                LOGIN => {
                    session.disconnect("Bedrock players can't play here yet - join with Java Edition for now.").await?;
                    return Ok(());
                }
                id => debug!("{peer}: ignoring packet {id}"),
            }
        }
    }
}

fn network_settings() -> Packet {
    let mut body = Vec::with_capacity(10);
    body.extend_from_slice(&COMPRESSION_THRESHOLD.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes()); // zlib (raw deflate)
    body.push(0); // client throttle off
    body.push(0);
    body.extend_from_slice(&0f32.to_le_bytes());
    Packet::new(NETWORK_SETTINGS, body)
}
