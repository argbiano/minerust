// Connects to a Bedrock server over NetherNet the way the game does and walks through
// RequestNetworkSettings -> NetworkSettings, optionally followed by a Login.
//
//   cargo run --release --example bedrock_probe -- 127.0.0.1:19132 [--login]
//
// Works against a stock Bedrock Dedicated Server too (online-mode=false), which is how
// the transport code here was checked.

use anyhow::{Context, Result, anyhow, bail, ensure};
use bytes::BytesMut;
use minerust::bedrock::{
    PROTOCOL_VERSION,
    batch::{self, Framing, Packet},
    framing::{self, Reassembler},
    identity,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::timeout,
};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceGatheringState, RTCSessionDescription,
    },
};

struct Gathered(mpsc::Sender<()>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Gathered {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.0.try_send(());
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:19132".into());
    let send_login = args.any(|a| a == "--login");

    let (status, body) = http(&addr, "GET", "/v1/join", None).await?;
    println!("GET /v1/join -> {status} {body}");
    ensure!((200..300).contains(&status), "server does not do NetherNet");

    let (gathered_tx, mut gathered) = mpsc::channel(1);
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_handler(Arc::new(Gathered(gathered_tx)))
        .with_runtime(webrtc::runtime::default_runtime().context("no runtime")?)
        .with_udp_addrs(vec!["0.0.0.0:0".to_owned()])
        .build()
        .await?;
    let reliable = pc.create_data_channel("ReliableDataChannel", None).await?;
    let unreliable = pc
        .create_data_channel(
            "UnreliableDataChannel",
            Some(RTCDataChannelInit { ordered: false, max_retransmits: Some(0), ..Default::default() }),
        )
        .await?;

    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;
    timeout(Duration::from_secs(5), gathered.recv()).await.context("ICE gathering timed out")?;
    let offer = pc.local_description().await.context("no offer")?;

    let network_id = std::env::var("PROBE_NETWORK_ID").unwrap_or_else(|_| rand::random::<u64>().to_string());
    let offer_sdp = if std::env::var_os("PROBE_SIGN_OFFER").is_some() {
        identity::OperatorKey::generate("probe").sign_answer(&offer.sdp)?
    } else {
        offer.sdp.clone()
    };
    let (status, answer) = http(&addr, "POST", &format!("/v1/join/{network_id}"), Some(&offer_sdp)).await?;
    ensure!((200..300).contains(&status), "signaling refused the offer: {status} {answer}");

    if std::env::var_os("PROBE_DUMP_SDP").is_some() {
        println!("--- offer\n{}--- answer\n{answer}---", offer.sdp);
    }
    let id = identity::verify_self_signed(&answer).context("server identity")?;
    println!("server identity ok: domain={:?} claims={}", id.domain, id.claims);
    pc.set_remote_description(RTCSessionDescription::answer(identity::strip_identity(&answer))?).await?;

    wait_open(&reliable).await?;
    println!("ReliableDataChannel open");
    let _keep = unreliable;

    let mut framing_mode = Framing::Raw;
    let request = Packet::new(193, PROTOCOL_VERSION.to_be_bytes().to_vec());
    send(&reliable, &[request], framing_mode).await?;

    let mut reassembler = Reassembler::default();
    loop {
        let event = timeout(Duration::from_secs(10), reliable.poll()).await.context("no reply")?;
        let data = match event {
            Some(DataChannelEvent::OnMessage(msg)) => msg.data.to_vec(),
            Some(DataChannelEvent::OnClose) | None => bail!("data channel closed"),
            Some(_) => continue,
        };
        let Some(data) = reassembler.push(&data)? else { continue };
        for p in batch::decode(&data, framing_mode)? {
            match p.id {
                143 => {
                    let threshold = u16::from_le_bytes([p.body[0], p.body[1]]);
                    let algorithm = u16::from_le_bytes([p.body[2], p.body[3]]);
                    println!("NetworkSettings: threshold={threshold} algorithm={algorithm}");
                    ensure!(algorithm == 0, "probe only speaks deflate");
                    framing_mode = Framing::Deflate { threshold: threshold.max(1) as usize };
                    if !send_login {
                        pc.close().await?;
                        return Ok(());
                    }
                    // Not a real login: enough to see how the server reacts.
                    let mut body = PROTOCOL_VERSION.to_be_bytes().to_vec();
                    body.push(0);
                    send(&reliable, &[Packet::new(1, body)], framing_mode).await?;
                }
                2 => println!("PlayStatus: {}", i32::from_be_bytes(p.body[..4].try_into()?)),
                5 => {
                    println!("Disconnect: {}", disconnect_message(&p.body).unwrap_or_default());
                    pc.close().await?;
                    return Ok(());
                }
                id => println!("packet {id} ({} bytes)", p.body.len()),
            }
        }
    }
}

async fn send(dc: &Arc<dyn DataChannel>, packets: &[Packet], framing_mode: Framing) -> Result<()> {
    for fragment in framing::split(&batch::encode(packets, framing_mode)?)? {
        dc.send(BytesMut::from(&fragment[..])).await?;
    }
    Ok(())
}

async fn wait_open(dc: &Arc<dyn DataChannel>) -> Result<()> {
    let opened = async {
        while let Some(event) = dc.poll().await {
            if matches!(event, DataChannelEvent::OnOpen) {
                return Ok(());
            }
        }
        Err(anyhow!("channel closed before opening"))
    };
    timeout(Duration::from_secs(15), opened).await.context("data channel never opened")?
}

fn disconnect_message(body: &[u8]) -> Result<String> {
    let mut pos = 0;
    batch::get_varuint(body, &mut pos)?;
    ensure!(body.get(pos) == Some(&0), "no message");
    pos += 1;
    let len = batch::get_varuint(body, &mut pos)? as usize;
    Ok(String::from_utf8_lossy(body.get(pos..pos + len).context("short message")?).into_owned())
}

async fn http(addr: &str, method: &str, path: &str, body: Option<&str>) -> Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr).await?;
    let body = body.unwrap_or("");
    let content_type = if method == "POST" { "Content-Type: application/sdp\r\n" } else { "" };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let response = String::from_utf8_lossy(&response);
    let (head, body) = response.split_once("\r\n\r\n").context("bad HTTP response")?;
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).context("bad status line")?;
    Ok((status, body.to_owned()))
}
