// Wire primitives and packet framing for the Java Edition protocol.

use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use std::io::{Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

/// Largest frame vanilla accepts (3-byte varint length).
pub const MAX_FRAME: usize = 2_097_151;
/// Largest decompressed packet vanilla accepts.
pub const MAX_UNCOMPRESSED: usize = 8_388_608;

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn varlong(&mut self, value: i64) -> &mut Self {
        let mut v = value as u64;
        while v >= 0x80 {
            self.buf.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
        self
    }

    pub fn varint(&mut self, value: i32) -> &mut Self {
        write_varint(&mut self.buf, value);
        self
    }

    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.buf.push(value);
        self
    }

    pub fn bool(&mut self, value: bool) -> &mut Self {
        self.u8(value as u8)
    }

    pub fn i16(&mut self, value: i16) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn i32(&mut self, value: i32) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn i64(&mut self, value: i64) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn f32(&mut self, value: f32) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn f64(&mut self, value: f64) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn string(&mut self, value: &str) -> &mut Self {
        self.varint(value.len() as i32);
        self.bytes(value.as_bytes())
    }

    pub fn uuid(&mut self, value: Uuid) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(value);
        self
    }

    /// A text component as network NBT: a nameless root `TAG_String`.
    pub fn nbt_text(&mut self, text: &str) -> &mut Self {
        let encoded = modified_utf8(text);
        self.u8(8).bytes(&(encoded.len() as u16).to_be_bytes()).bytes(&encoded)
    }

    /// `{text, color}` as an anonymous NBT compound.
    pub fn nbt_colored_text(&mut self, text: &str, color: &str) -> &mut Self {
        self.u8(10);
        for (key, value) in [("text", text), ("color", color)] {
            let (key, value) = (modified_utf8(key), modified_utf8(value));
            self.u8(8).bytes(&(key.len() as u16).to_be_bytes()).bytes(&key);
            self.bytes(&(value.len() as u16).to_be_bytes()).bytes(&value);
        }
        self.u8(0)
    }
}

pub fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            buf.push(v as u8);
            return;
        }
        buf.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

/// Java's modified UTF-8, used by NBT strings.
fn modified_utf8(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for unit in text.encode_utf16() {
        match unit {
            0x0001..=0x007F => out.push(unit as u8),
            0x0000 | 0x0080..=0x07FF => {
                out.push(0xC0 | (unit >> 6) as u8);
                out.push(0x80 | (unit & 0x3F) as u8);
            }
            _ => {
                out.push(0xE0 | (unit >> 12) as u8);
                out.push(0x80 | ((unit >> 6) & 0x3F) as u8);
                out.push(0x80 | (unit & 0x3F) as u8);
            }
        }
    }
    out
}

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.pos + n <= self.data.len(), "unexpected end of packet");
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.bytes(8)?.try_into()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.bytes(8)?.try_into()?))
    }

    pub fn varint(&mut self) -> Result<i32> {
        let mut result = 0u32;
        for i in 0..5 {
            let b = self.u8()?;
            result |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(result as i32);
            }
        }
        bail!("varint too long")
    }

    pub fn string(&mut self, max_chars: usize) -> Result<String> {
        let len = self.varint()?;
        ensure!(len >= 0 && len as usize <= max_chars * 3, "string too long");
        let s = std::str::from_utf8(self.bytes(len as usize)?).context("invalid utf-8")?;
        ensure!(s.chars().count() <= max_chars, "string too long");
        Ok(s.to_owned())
    }
}

async fn read_varint_async<R: AsyncRead + Unpin>(r: &mut R) -> Result<i32> {
    let mut result = 0u32;
    for i in 0..3 {
        let b = r.read_u8().await?;
        result |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    bail!("frame length varint too long")
}

pub struct RawPacket {
    pub id: i32,
    pub body: Vec<u8>,
}

/// Reads one frame. `compression` is the negotiated threshold, if any.
pub async fn read_packet<R: AsyncRead + Unpin>(r: &mut R, compression: Option<usize>) -> Result<RawPacket> {
    let len = read_varint_async(r).await?;
    ensure!(len > 0 && len as usize <= MAX_FRAME, "bad frame length {len}");
    let mut frame = vec![0u8; len as usize];
    r.read_exact(&mut frame).await?;

    let data = match compression {
        None => frame,
        Some(threshold) => {
            let mut rd = Reader::new(&frame);
            let data_len = rd.varint()?;
            if data_len == 0 {
                rd.remaining().to_vec()
            } else {
                let data_len = data_len as usize;
                ensure!(data_len >= threshold, "badly compressed packet: {data_len} below threshold");
                ensure!(data_len <= MAX_UNCOMPRESSED, "packet too large: {data_len}");
                let mut out = Vec::with_capacity(data_len);
                ZlibDecoder::new(rd.remaining()).take(data_len as u64 + 1).read_to_end(&mut out)?;
                ensure!(out.len() == data_len, "decompressed length mismatch");
                out
            }
        }
    };
    let mut rd = Reader::new(&data);
    let id = rd.varint()?;
    let body = rd.remaining().to_vec();
    Ok(RawPacket { id, body })
}

pub fn encode_packet(id: i32, body: &[u8], compression: Option<usize>) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(body.len() + 5);
    write_varint(&mut data, id);
    data.extend_from_slice(body);

    let payload = match compression {
        None => data,
        Some(threshold) if data.len() < threshold => {
            let mut p = Vec::with_capacity(data.len() + 1);
            p.push(0);
            p.extend_from_slice(&data);
            p
        }
        Some(_) => {
            let mut p = Vec::new();
            write_varint(&mut p, data.len() as i32);
            let mut enc = ZlibEncoder::new(p, Compression::default());
            enc.write_all(&data)?;
            enc.finish()?
        }
    };
    ensure!(payload.len() <= MAX_FRAME, "outgoing packet {id:#x} too large ({} bytes)", payload.len());
    let mut frame = Vec::with_capacity(payload.len() + 3);
    write_varint(&mut frame, payload.len() as i32);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub async fn write_packet<W: AsyncWrite + Unpin>(
    w: &mut W,
    id: i32,
    body: &[u8],
    compression: Option<usize>,
) -> Result<()> {
    w.write_all(&encode_packet(id, body, compression)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0, 1, 127, 128, 255, 25565, 2_097_151, i32::MAX, -1, i32::MIN] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            assert_eq!(Reader::new(&buf).varint().unwrap(), v);
        }
        let mut buf = Vec::new();
        write_varint(&mut buf, -1);
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
    }

    #[tokio::test]
    async fn compressed_frame_roundtrip() {
        for (size, threshold) in [(10, 256), (5000, 256)] {
            let body = vec![7u8; size];
            let frame = encode_packet(0x2A, &body, Some(threshold)).unwrap();
            let pkt = read_packet(&mut frame.as_slice(), Some(threshold)).await.unwrap();
            assert_eq!(pkt.id, 0x2A);
            assert_eq!(pkt.body, body);
        }
    }
}
