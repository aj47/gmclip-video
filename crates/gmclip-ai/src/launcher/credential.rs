//! The run credential (R-AGT-5): 256 bits from the OS, held in memory, handed to the child
//! only through `GMCLIP_RUN`, never printed, logged, recorded or written to a file.
//!
//! The OS source is `/dev/urandom` (no new crate, SR R-SUP-1). M1 is macOS-only; other
//! platforms return an error until they need the launcher.

use std::fmt;

/// 32 random bytes. `Debug` never shows them; dropping overwrites them (best effort: no
/// `unsafe` volatile write is allowed in this crate).
pub struct RunCredential([u8; 32]);

impl RunCredential {
    /// Mints a fresh credential.
    pub fn mint() -> std::io::Result<Self> {
        let mut b = [0u8; 32];
        fill_random(&mut b)?;
        if b.iter().all(|&x| x == 0) {
            return Err(std::io::Error::other("the OS random source returned zeros"));
        }
        Ok(Self(b))
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Lowercase hex, for the child's environment and the holder registration only.
    pub fn expose_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A non-secret fingerprint for the run record: sha256 of the credential, first 8 bytes.
    pub fn fingerprint(&self) -> String {
        super::sha256::hex(&self.0)[..16].to_owned()
    }
}

impl fmt::Debug for RunCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RunCredential(<redacted>)")
    }
}

impl Drop for RunCredential {
    fn drop(&mut self) {
        self.0 = [0; 32];
        std::hint::black_box(&self.0);
    }
}

/// Fills `buf` from the OS random source.
pub fn fill_random(buf: &mut [u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(buf)
    }
    #[cfg(windows)]
    {
        gmclip_platform::windows::fill_random(buf)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = buf;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no OS random source on this platform",
        ))
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_256_bit_distinct_and_redacted() {
        let a = RunCredential::mint().unwrap();
        let b = RunCredential::mint().unwrap();
        assert_eq!(a.expose_hex().len(), 64);
        assert_ne!(a.expose_hex(), b.expose_hex());
        assert_eq!(format!("{a:?}"), "RunCredential(<redacted>)");
        assert_eq!(a.fingerprint().len(), 16);
    }
}
