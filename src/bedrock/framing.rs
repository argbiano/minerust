// NetherNet prefixes every data-channel message with one byte: how many fragments
// are still to come. Only ReliableDataChannel ever carries more than one.

use anyhow::{Result, bail, ensure};

/// Below the max-message-size Minecraft advertises (262144), with room to spare.
pub const MAX_FRAGMENT: usize = 64 * 1024 - 1;

pub fn split(payload: &[u8]) -> Result<Vec<Vec<u8>>> {
    let chunks: Vec<&[u8]> = if payload.is_empty() { vec![&[][..]] } else { payload.chunks(MAX_FRAGMENT).collect() };
    ensure!(chunks.len() <= 256, "message too large to fragment ({} bytes)", payload.len());
    let mut remaining = chunks.len();
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            remaining -= 1;
            let mut msg = Vec::with_capacity(chunk.len() + 1);
            msg.push(remaining as u8);
            msg.extend_from_slice(chunk);
            msg
        })
        .collect())
}

#[derive(Default)]
pub struct Reassembler {
    buf: Vec<u8>,
    expected: Option<u8>,
}

impl Reassembler {
    pub fn push(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>> {
        let Some((&left, data)) = msg.split_first() else { bail!("empty NetherNet message") };
        if let Some(expected) = self.expected {
            ensure!(left == expected, "fragment out of order: got {left}, expected {expected}");
        }
        ensure!(self.buf.len() + data.len() <= super::batch::MAX_BATCH, "reassembled message too large");
        self.buf.extend_from_slice(data);
        if left == 0 {
            self.expected = None;
            Ok(Some(std::mem::take(&mut self.buf)))
        } else {
            self.expected = Some(left - 1);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_message_is_one_fragment() {
        assert_eq!(split(b"hi").unwrap(), vec![vec![0, b'h', b'i']]);
    }

    #[test]
    fn split_and_reassemble() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let parts = split(&payload).unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts.iter().map(|p| p[0]).collect::<Vec<_>>(), [3, 2, 1, 0]);

        let mut r = Reassembler::default();
        let mut out = None;
        for p in &parts {
            out = r.push(p).unwrap();
        }
        assert_eq!(out.unwrap(), payload);
    }

    #[test]
    fn rejects_out_of_order() {
        let mut r = Reassembler::default();
        assert!(r.push(&[2, 1]).unwrap().is_none());
        assert!(r.push(&[0, 2]).is_err());
    }
}
