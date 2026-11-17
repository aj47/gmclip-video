//! The local transport under [`crate::wire`]: a Unix socket in a 0700 per-user directory
//! (macOS, Linux), a named pipe with an owner-only DACL on Windows (decisions/0019).
//!
//! Both answer the same two questions before any byte of the handshake: is the peer
//! this user, and which process is it. Unix asks the kernel for the socket peer's
//! credentials (`getpeereid` / `SO_PEERCRED`); Windows asks for the pipe peer's process
//! id (`GetNamedPipeClientProcessId` / `GetNamedPipeServerProcessId`) and that process's
//! token user SID. The rest of the exchange (nonces, HMAC proofs, the executable check)
//! is the same code on every OS.

/// Who is at the other end, from the OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// This user; the pid when the OS says.
    Ours(Option<u32>),
    /// Another user.
    Other(Option<u32>),
}

#[cfg(unix)]
mod imp {
    use super::Peer;

    pub type Listener = tokio::net::UnixListener;
    /// What a server accepts.
    pub type ServerStream = tokio::net::UnixStream;
    /// What a client connects with.
    pub type ClientStream = tokio::net::UnixStream;
    pub type ReadHalf = tokio::net::unix::OwnedReadHalf;
    pub type WriteHalf = tokio::net::unix::OwnedWriteHalf;

    /// The next connection.
    pub async fn accept(l: &mut Listener) -> std::io::Result<ServerStream> {
        l.accept().await.map(|(s, _)| s)
    }

    /// The peer of an accepted or connected socket; `None` if the kernel cannot say.
    pub fn peer(s: &tokio::net::UnixStream) -> Option<Peer> {
        let cred = s.peer_cred().ok()?;
        let pid = cred.pid().and_then(|p| u32::try_from(p).ok());
        Some(if cred.uid() == crate::paths::our_uid() {
            Peer::Ours(pid)
        } else {
            Peer::Other(pid)
        })
    }

    pub fn split(s: ClientStream) -> (ReadHalf, WriteHalf) {
        s.into_split()
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use gmclip_platform::windows::{self as win, pipe};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer};

    use super::Peer;

    /// `ERROR_PIPE_BUSY`: every instance is connected; wait for the next one.
    const ERROR_PIPE_BUSY: i32 = 231;

    /// One pipe name and the instance waiting for the next client. A new instance is
    /// created as each one connects, so a client never finds the name missing while the
    /// holder runs.
    #[derive(Debug)]
    pub struct Listener {
        name: OsString,
        next: NamedPipeServer,
    }

    impl Listener {
        /// The first instance of `name`: refused if the name exists (another holder, or
        /// a squatter: decisions/0019).
        pub fn bind(name: &Path) -> std::io::Result<Listener> {
            let name = name.as_os_str().to_owned();
            let next = pipe::create_server(&name, true)?;
            Ok(Listener { name, next })
        }
    }

    pub type ServerStream = NamedPipeServer;
    pub type ClientStream = NamedPipeClient;
    pub type ReadHalf = tokio::io::ReadHalf<NamedPipeClient>;
    pub type WriteHalf = tokio::io::WriteHalf<NamedPipeClient>;

    /// The next connection. Cancel-safe: dropped while waiting, the instance stays.
    /// An instance whose client came and went before the connect completed is replaced
    /// (the fresh one exists before the old closes, so the name never lapses).
    pub async fn accept(l: &mut Listener) -> std::io::Result<ServerStream> {
        match l.next.connect().await {
            Ok(()) => {
                let fresh = pipe::create_server(&l.name, false)?;
                Ok(std::mem::replace(&mut l.next, fresh))
            }
            Err(e) => {
                l.next = pipe::create_server(&l.name, false)?;
                Err(e)
            }
        }
    }

    /// `None` when the OS cannot say (the peer already exited, or its token cannot be
    /// read): the connection is dropped without an answer, as on Unix.
    fn peer_of(pid: std::io::Result<u32>) -> Option<Peer> {
        let pid = pid.ok()?;
        let theirs = win::process_user_sid(pid).ok()?;
        let ours = win::current_user_sid().ok()?;
        Some(if theirs == ours {
            Peer::Ours(Some(pid))
        } else {
            Peer::Other(Some(pid))
        })
    }

    /// The client of an accepted pipe instance.
    pub fn peer(s: &NamedPipeServer) -> Option<Peer> {
        peer_of(pipe::client_pid(s))
    }

    /// The server a client is connected to.
    pub fn server_peer(c: &NamedPipeClient) -> Option<Peer> {
        peer_of(pipe::server_pid(c))
    }

    /// Opens the pipe, waiting up to 2 s while every instance is busy. Never follows
    /// the name anywhere else: `\\.\pipe\` is its own namespace.
    pub async fn open(name: &Path) -> std::io::Result<ClientStream> {
        let t0 = Instant::now();
        loop {
            match ClientOptions::new().open(name) {
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                        && t0.elapsed() < Duration::from_secs(2) =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                r => return r,
            }
        }
    }

    pub fn split(s: ClientStream) -> (ReadHalf, WriteHalf) {
        tokio::io::split(s)
    }
}

pub use imp::*;
