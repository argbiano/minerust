// Bedrock game packets travel in batches: each packet is length-prefixed, and once
// NetworkSettings has been exchanged the whole batch gets a compression-algorithm byte.

use anyhow::{Result, bail, ensure};
use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};
use std::io::{Read, Write};

pub const MAX_BATCH: usize = 16 * 1024 * 1024;

const ALGO_DEFLATE: u8 = 0x00;
const ALGO_SNAPPY: u8 = 0x01;
const ALGO_NONE: u8 = 0xFF;

#[derive(Clone, Copy, Debug)]
pub enum Framing {
    /// Before NetworkSettings: no algorithm byte at all.
    Raw,
    /// Raw DEFLATE for batches of at least `threshold` bytes.
    Deflate { threshold: usize },
}

pub struct Packet {
    pub id: u32,
    pub body: Vec<u8>,
}

impl Packet {
    pub fn new(id: u32, body: Vec<u8>) -> Self {
        Self { id, body }
    }
}

pub fn encode(packets: &[Packet], framing: Framing) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    for p in packets {
        let mut header = Vec::with_capacity(2);
        // Sub-client ids live in bits 10..14; we never use split screen.
        put_varuint(&mut header, p.id & 0x3FF);
        put_varuint(&mut raw, (header.len() + p.body.len()) as u32);
        raw.extend_from_slice(&header);
        raw.extend_from_slice(&p.body);
    }

    Ok(match framing {
        Framing::Raw => raw,
        Framing::Deflate { threshold } if raw.len() < threshold => {
            let mut out = Vec::with_capacity(raw.len() + 1);
            out.push(ALGO_NONE);
            out.extend_from_slice(&raw);
            out
        }
        Framing::Deflate { .. } => {
            let mut enc = DeflateEncoder::new(vec![ALGO_DEFLATE], Compression::default());
            enc.write_all(&raw)?;
            enc.finish()?
        }
    })
}

pub fn decode(data: &[u8], framing: Framing) -> Result<Vec<Packet>> {
    let inflated;
    let raw = match framing {
        Framing::Raw => data,
        Framing::Deflate { .. } => {
            let (&algo, rest) = data.split_first().ok_or_else(|| anyhow::anyhow!("empty batch"))?;
            match algo {
                ALGO_NONE => rest,
                ALGO_DEFLATE => {
                    let mut out = Vec::new();
                    DeflateDecoder::new(rest).take(MAX_BATCH as u64 + 1).read_to_end(&mut out)?;
                    ensure!(out.len() <= MAX_BATCH, "batch too large");
                    inflated = out;
                    &inflated
                }
                ALGO_SNAPPY => bail!("client used snappy, but we negotiated deflate"),
                other => bail!("unknown compression algorithm {other:#x}"),
            }
        }
    };

    let mut packets = Vec::new();
    let mut pos = 0;
    while pos < raw.len() {
        let len = get_varuint(raw, &mut pos)? as usize;
        ensure!(pos + len <= raw.len(), "packet runs past the end of the batch");
        let mut inner = pos;
        let header = get_varuint(&raw[..pos + len], &mut inner)?;
        packets.push(Packet::new(header & 0x3FF, raw[inner..pos + len].to_vec()));
        pos += len;
    }
    Ok(packets)
}

pub fn put_varuint(buf: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        buf.push(v as u8 | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

pub fn put_varint(buf: &mut Vec<u8>, v: i32) {
    put_varuint(buf, ((v << 1) ^ (v >> 31)) as u32);
}

pub fn put_string(buf: &mut Vec<u8>, s: &str) {
    put_varuint(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
}

pub fn get_varuint(buf: &[u8], pos: &mut usize) -> Result<u32> {
    let mut v = 0u32;
    for shift in (0..35).step_by(7) {
        let Some(&b) = buf.get(*pos) else { bail!("truncated varint") };
        *pos += 1;
        v |= ((b & 0x7F) as u32) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    bail!("varint too long")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag() {
        let mut b = Vec::new();
        put_varint(&mut b, -1);
        put_varint(&mut b, 55);
        assert_eq!(b, [1, 110]);
    }

    #[test]
    fn round_trip() {
        let packets = [Packet::new(193, 2225i32.to_be_bytes().to_vec()), Packet::new(1, vec![7; 3000])];
        for framing in [Framing::Raw, Framing::Deflate { threshold: 256 }, Framing::Deflate { threshold: 1 << 20 }] {
            let data = encode(&packets, framing).unwrap();
            let back = decode(&data, framing).unwrap();
            assert_eq!(back.len(), 2);
            assert_eq!(back[0].id, 193);
            assert_eq!(back[1].body, vec![7; 3000]);
        }
    }

    #[test]
    fn request_network_settings_wire_bytes() {
        let data = encode(&[Packet::new(193, 2225i32.to_be_bytes().to_vec())], Framing::Raw).unwrap();
        // length 6, header 193 as two varint bytes, then big-endian 2225
        assert_eq!(data, [0x06, 0xC1, 0x01, 0x00, 0x00, 0x08, 0xB1]);
    }
}
