//! The socket's opening exchange (R-ARCH-5a, R-AGT-5), before any MCP byte:
//!
//! 1. The server checks the peer's uid (`getpeereid`/`SO_PEERCRED`) and drops any other
//!    user, then sends `Hello {server_nonce}`.
//! 2. The client checks the *server* (uid ours, executable the installed gmclip) and only
//!    then sends `ClientHello {mode, client_nonce, run?, proof?, session?}`. A launched run proves
//!    its credential as `HMAC-SHA256(credential, server_nonce ‖ client_nonce)`; the
//!    credential itself is never sent.
//! 3. The server answers `Welcome {identity}` (or an error) and the connection becomes
//!    MCP JSON-RPC (`mode: mcp`) or line-delimited control requests (`mode: control`:
//!    status, connections, Owner calls; never listed by `tools/list`).

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Wire version of this exchange.
pub const VERSION: u32 = 1;
/// No handshake line is longer.
pub const MAX_LINE: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub gmclip: u32,
    pub server_nonce: String,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Mcp,
    Control,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientHello {
    pub mode: Mode,
    pub client_nonce: String,
    /// A launched run's id (not secret).
    #[serde(default)]
    pub run: Option<String>,
    /// `HMAC(credential, server_nonce ‖ client_nonce)`, hex.
    #[serde(default)]
    pub proof: Option<String>,
    /// What launched the shim (its parent's executable), for the connection log.
    #[serde(default)]
    pub client_exe: Option<String>,
    /// An agent session's label (`GMCLIP_SESSION`, decision 0050): every connection that
    /// names it is one identity with one run. Not a credential: it widens nothing, and a
    /// launched run's proof wins over it. Sent only when set (an older holder refuses
    /// unknown fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// A session label as given (`GMCLIP_SESSION`): 1–64 letters, digits, `.`, `_` or `-`.
pub fn session_label(s: &str) -> Option<&str> {
    ((1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
    .then_some(s)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Welcome {
    pub ok: bool,
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// 32 random bytes, hex.
pub fn nonce() -> String {
    let mut b = [0u8; 32];
    // getrandom failing means no OS entropy: refuse rather than use a weak nonce
    getrandom::fill(&mut b).expect("OS randomness");
    hex(&b)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || s.len() > 1024 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn mac(credential: &[u8], server_nonce: &str, client_nonce: &str) -> Hmac<Sha256> {
    let mut m =
        <Hmac<Sha256> as Mac>::new_from_slice(credential).expect("HMAC takes any key length");
    m.update(server_nonce.as_bytes());
    m.update(b"\x00");
    m.update(client_nonce.as_bytes());
    m
}

/// The proof a launched run sends.
pub fn proof(credential: &[u8], server_nonce: &str, client_nonce: &str) -> String {
    hex(&mac(credential, server_nonce, client_nonce)
        .finalize()
        .into_bytes())
}

/// Constant-time check of a proof.
pub fn verify(credential: &[u8], server_nonce: &str, client_nonce: &str, proof_hex: &str) -> bool {
    let Some(p) = unhex(proof_hex) else {
        return false;
    };
    mac(credential, server_nonce, client_nonce)
        .verify_slice(&p)
        .is_ok()
}

/// `GMCLIP_RUN` = `<run id>:<credential hex>`.
pub fn parse_run_env(v: &str) -> Option<(String, Vec<u8>)> {
    let (run, cred) = v.split_once(':')?;
    let cred = unhex(cred)?;
    (cred.len() >= 32 && !run.is_empty() && run.len() <= 64).then(|| (run.to_owned(), cred))
}

/// Reads one `\n`-terminated line byte by byte (so nothing after it is consumed).
pub async fn read_line<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(256);
    loop {
        let b = r.read_u8().await?;
        if b == b'\n' {
            break;
        }
        buf.push(b);
        if buf.len() > MAX_LINE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
    }
    String::from_utf8(buf)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not UTF-8"))
}

/// Writes one JSON value as a line.
pub async fn write_json<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    v: &T,
) -> std::io::Result<()> {
    let mut s = serde_json::to_string(v).map_err(std::io::Error::other)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await
}

/// Reads one JSON line.
pub async fn read_json<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    r: &mut R,
) -> std::io::Result<T> {
    let line = read_line(r).await?;
    serde_json::from_str(&line)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed handshake"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proofs_bind_both_nonces() {
        let cred = [7u8; 32];
        let (s, c) = (nonce(), nonce());
        let p = proof(&cred, &s, &c);
        assert!(verify(&cred, &s, &c, &p));
        assert!(!verify(&cred, &nonce(), &c, &p), "another server nonce");
        assert!(!verify(&cred, &s, &nonce(), &p), "another client nonce");
        assert!(!verify(&[8u8; 32], &s, &c, &p), "another credential");
        assert!(!verify(&cred, &s, &c, "zz"));
        assert!(!p.contains(&hex(&cred)), "the credential never appears");
    }

    #[test]
    fn run_env_parses() {
        let v = format!("01J0RUN:{}", hex(&[1u8; 32]));
        assert_eq!(parse_run_env(&v).unwrap().0, "01J0RUN");
        assert!(parse_run_env("x:abcd").is_none(), "short credential");
        assert!(parse_run_env("nocolon").is_none());
    }
}
