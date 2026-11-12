// NetherNet `a=identity` assertions (RFC 8827 §5 envelope). The server signs every
// SDP answer with a long-lived operator key; clients pin that key on first use when
// signaling runs over plain HTTP.

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use p384::ecdsa::{
    Signature, SigningKey, VerifyingKey,
    signature::{Signer, Verifier},
};
use p384::elliptic_curve::Generate;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const TOKEN_LIFETIME_SECS: u64 = 24 * 60 * 60;

pub struct OperatorKey {
    key: SigningKey,
    domain: String,
}

impl OperatorKey {
    /// The key has to survive restarts, or every returning player sees the trust prompt again.
    pub fn load_or_create(path: &Path, domain: &str) -> Result<Self> {
        let key = match std::fs::read(path) {
            Ok(bytes) => {
                SigningKey::from_slice(&bytes).with_context(|| format!("{} is not a P-384 key", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = SigningKey::generate_from_rng(&mut rand::rng());
                write_private(path, &key.to_bytes())?;
                key
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { key, domain: domain.to_owned() })
    }

    pub fn generate(domain: &str) -> Self {
        Self { key: SigningKey::generate_from_rng(&mut rand::rng()), domain: domain.to_owned() }
    }

    /// SHA-256 of the public key, for players who want to check the trust prompt out of band.
    pub fn fingerprint(&self) -> String {
        let point = self.key.verifying_key().to_sec1_point(false);
        hex_colon(&Sha256::digest(point.as_bytes()))
    }

    pub fn sign_answer(&self, sdp: &str) -> Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let claims = json!({
            "cpk": jwk(self.key.verifying_key()),
            "iss": self.domain,
            "iat": now,
            "exp": now + TOKEN_LIFETIME_SECS,
        });
        let token = self.jws(&json!({ "alg": "ES384", "typ": "JWT" }), claims.to_string().as_bytes());
        let signed = self.jws(&json!({ "alg": "ES384" }), fingerprint_payload(sdp)?.as_bytes());
        let (header, _, sig) = split_jws(&signed)?;
        let assertion = json!({ "token": token, "fingerprints": format!("{header}..{sig}") }).to_string();
        let envelope = json!({
            "idp": { "domain": self.domain, "protocol": "default" },
            "assertion": assertion,
        });
        insert_identity(sdp, &STANDARD.encode(envelope.to_string()))
    }

    fn jws(&self, header: &Value, payload: &[u8]) -> String {
        let input = format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(payload));
        let sig: Signature = self.key.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }
}

#[derive(Debug)]
pub struct Identity {
    pub domain: String,
    pub claims: Value,
}

/// Checks a self-signed assertion the way the Minecraft client checks a server answer:
/// the token is signed by its own `cpk`, and so are the SDP's fingerprints.
pub fn verify_self_signed(sdp: &str) -> Result<Identity> {
    let (domain, token, fingerprints) = parse_identity(sdp)?;
    let (h, p, s) = split_jws(&token)?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p)?).context("token claims")?;
    let key = verifying_key_from_jwk(claims.get("cpk").ok_or_else(|| anyhow!("token has no cpk"))?)?;
    verify_es384(&key, h, p, s).context("token signature")?;

    let (h, empty, s) = split_jws(&fingerprints)?;
    ensure!(empty.is_empty(), "fingerprint JWS is not detached");
    let payload = URL_SAFE_NO_PAD.encode(fingerprint_payload(sdp)?);
    verify_es384(&key, h, &payload, s).context("fingerprint signature")?;

    if let Some(exp) = claims.get("exp").and_then(Value::as_u64) {
        ensure!(exp > SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(), "token expired");
    }
    Ok(Identity { domain, claims })
}

/// WebRTC stacks reject the attribute, so it comes out before setRemoteDescription.
pub fn strip_identity(sdp: &str) -> String {
    sdp.split_inclusive('\n').filter(|l| !l.starts_with("a=identity:")).collect()
}

pub fn has_identity(sdp: &str) -> bool {
    sdp.lines().any(|l| l.starts_with("a=identity:"))
}

fn parse_identity(sdp: &str) -> Result<(String, String, String)> {
    let line =
        sdp.lines().find_map(|l| l.strip_prefix("a=identity:")).ok_or_else(|| anyhow!("no a=identity in SDP"))?;
    let raw = line.trim();
    let envelope = STANDARD.decode(raw).or_else(|_| STANDARD_NO_PAD.decode(raw)).context("identity is not base64")?;
    let envelope: Value = serde_json::from_slice(&envelope).context("identity is not JSON")?;
    let domain = envelope["idp"]["domain"].as_str().unwrap_or_default().to_owned();
    let assertion: Value =
        serde_json::from_str(envelope["assertion"].as_str().ok_or_else(|| anyhow!("no assertion"))?)?;
    let get = |k: &str| assertion[k].as_str().map(str::to_owned).ok_or_else(|| anyhow!("assertion has no {k}"));
    Ok((domain, get("token")?, get("fingerprints")?))
}

/// Canonical JSON of the SDP's a=fingerprint lines, in order of appearance.
fn fingerprint_payload(sdp: &str) -> Result<String> {
    let list: Vec<Value> = sdp
        .lines()
        .filter_map(|l| l.strip_prefix("a=fingerprint:"))
        .map(|f| {
            let (algorithm, digest) = f.trim().split_once(' ').unwrap_or((f.trim(), ""));
            json!({ "algorithm": algorithm, "digest": digest })
        })
        .collect();
    ensure!(!list.is_empty(), "SDP has no a=fingerprint");
    // serde_json without preserve_order keeps keys sorted and adds no whitespace.
    Ok(json!({ "fingerprint": list }).to_string())
}

fn insert_identity(sdp: &str, value: &str) -> Result<String> {
    let at = sdp.find("\nm=").map(|i| i + 1).ok_or_else(|| anyhow!("SDP has no media section"))?;
    let eol = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    Ok(format!("{}a=identity:{value}{eol}{}", &sdp[..at], &sdp[at..]))
}

fn split_jws(jws: &str) -> Result<(&str, &str, &str)> {
    let mut parts = jws.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => Ok((h, p, s)),
        _ => bail!("malformed JWS"),
    }
}

fn verify_es384(key: &VerifyingKey, header: &str, payload: &str, sig: &str) -> Result<()> {
    let h: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header)?)?;
    ensure!(h["alg"] == "ES384", "unsupported JWS alg {}", h["alg"]);
    let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(sig)?)?;
    key.verify(format!("{header}.{payload}").as_bytes(), &sig)?;
    Ok(())
}

fn jwk(key: &VerifyingKey) -> Value {
    let point = key.to_sec1_point(false);
    let bytes = point.as_bytes();
    json!({
        "crv": "P-384",
        "kty": "EC",
        "x": URL_SAFE_NO_PAD.encode(&bytes[1..49]),
        "y": URL_SAFE_NO_PAD.encode(&bytes[49..97]),
    })
}

fn verifying_key_from_jwk(jwk: &Value) -> Result<VerifyingKey> {
    ensure!(jwk["kty"] == "EC" && jwk["crv"] == "P-384", "cpk is not a P-384 JWK: {jwk}");
    let coord = |k: &str| -> Result<Vec<u8>> {
        let v = URL_SAFE_NO_PAD.decode(jwk[k].as_str().ok_or_else(|| anyhow!("cpk has no {k}"))?)?;
        ensure!(v.len() == 48, "cpk {k} has {} bytes", v.len());
        Ok(v)
    };
    let mut sec1 = vec![0x04];
    sec1.extend(coord("x")?);
    sec1.extend(coord("y")?);
    Ok(VerifyingKey::from_sec1_bytes(&sec1)?)
}

fn hex_colon(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path).and_then(|mut f| f.write_all(bytes)).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
        a=fingerprint:sha-256 11:22:33\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
        a=fingerprint:sha-256 11:22:33\r\na=setup:active\r\n";

    fn key() -> OperatorKey {
        OperatorKey { key: SigningKey::generate_from_rng(&mut rand::rng()), domain: "minerust.test".into() }
    }

    #[test]
    fn canonical_fingerprint_json() {
        assert_eq!(
            fingerprint_payload("a=fingerprint:sha-256 AB:CD\r\n").unwrap(),
            r#"{"fingerprint":[{"algorithm":"sha-256","digest":"AB:CD"}]}"#
        );
    }

    #[test]
    fn signed_answer_verifies() {
        let signed = key().sign_answer(ANSWER).unwrap();
        let identity_line = signed.lines().position(|l| l.starts_with("a=identity:")).unwrap();
        let first_media = signed.lines().position(|l| l.starts_with("m=")).unwrap();
        assert!(identity_line < first_media);

        let id = verify_self_signed(&signed).unwrap();
        assert_eq!(id.domain, "minerust.test");
        assert_eq!(strip_identity(&signed), ANSWER);
    }

    #[test]
    fn tampered_fingerprint_fails() {
        let signed = key().sign_answer(ANSWER).unwrap().replacen("11:22:33", "44:55:66", 1);
        assert!(verify_self_signed(&signed).is_err());
    }

    #[test]
    fn foreign_key_fails() {
        let a = key().sign_answer(ANSWER).unwrap();
        let b = key().sign_answer(ANSWER).unwrap();
        let token_from_b = b.lines().find(|l| l.starts_with("a=identity:")).unwrap();
        let mixed: String = a
            .split_inclusive('\n')
            .map(|l| if l.starts_with("a=identity:") { format!("{token_from_b}\r\n") } else { l.to_owned() })
            .collect();
        assert!(verify_self_signed(&mixed).is_ok(), "b's identity is valid on its own");
        // Same fingerprints, so swapping whole identities is fine; swapping only the token is not.
        let (_, _, fp_a) = parse_identity(&a).unwrap();
        let (_, token_b, _) = parse_identity(&b).unwrap();
        let assertion = json!({ "token": token_b, "fingerprints": fp_a }).to_string();
        let envelope = json!({ "idp": { "domain": "x", "protocol": "default" }, "assertion": assertion });
        let forged = insert_identity(&strip_identity(&a), &STANDARD.encode(envelope.to_string())).unwrap();
        assert!(verify_self_signed(&forged).is_err());
    }
}
