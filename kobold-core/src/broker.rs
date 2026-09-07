//! The egress broker: the only way out of an adapter's network namespace.
//!
//! **The adapter gets no network at all.** `sandbox::Policy` denies it, and
//! what it gets instead is a unix socket bound into the sandbox. To reach a
//! host it asks, in one line, and Kobold decides.
//!
//! What this deliberately is *not* is a proxy for the provider's API. Kobold
//! never sees plaintext, never holds a second copy of the credential, and
//! knows nothing about the wire format on the other side -- **TLS stays end
//! to end**, established by the adapter, through a socket Kobold merely
//! splices. It answers exactly one question: *may this process reach host X*.
//! That is what keeps an adapter free to speak whatever its provider speaks
//! while still being unable to phone anywhere else.
//!
//! Denying the network also kills DNS inside the sandbox for free: the
//! resolver has nowhere to send a query, so the broker does the resolving and
//! a confined adapter cannot use DNS as a covert channel.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixListener, UnixStream};

/// The one line a client sends, and the two it can get back.
///
/// Text and line-oriented on purpose. The alternative -- a length-prefixed
/// binary frame -- would be marginally cheaper on a path that runs **once per
/// connection**, and would make the one thing anyone ever needs to do here,
/// read a transcript of what an adapter asked for, require a decoder.
pub const OK: &str = "OK\n";

/// A private directory that removes itself.
///
/// Hand-rolled rather than a `tempfile` dependency, because what is needed is
/// one directory with one socket in it and a `Drop` -- and the socket path
/// ends up in a Seatbelt profile and a bind mount, so being able to say
/// exactly how it is named is worth more here than the crate would be.
pub struct Hutch(PathBuf);

impl Hutch {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Hutch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Make one, named uniquely enough for concurrent adapters and concurrent
/// test binaries in the same process.
pub fn hutch() -> io::Result<Hutch> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NTH: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kobold-egress-{}-{}",
        std::process::id(),
        NTH.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    // The socket inside is the adapter's entire network; the directory that
    // holds it should not be traversable by anyone else either.
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(Hutch(dir))
}

/// A running broker. Dropping it stops accepting and removes the socket.
pub struct Broker {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Broker {
    /// The socket's path on the host, to be bound into the sandbox.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.task.abort();
        // Best effort: the socket is in a per-adapter temporary directory, so
        // a leftover node is inert. Removing it anyway keeps a second run
        // from tripping over `EADDRINUSE`.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether `host:port` is one the adapter is allowed to reach.
///
/// An entry is either a bare host, which grants **port 443 and nothing
/// else**, or an explicit `host:port`, which grants exactly that.
///
/// The bare form is the one anybody writes, and it is 443-only because this
/// exists to let an adapter reach an HTTPS API -- a plaintext port would mean
/// the credential leaving the machine unencrypted, and that should not be
/// something a user can do by accident. Naming a port is deliberate, visible
/// in the setting, and the only way to get anything else.
///
/// **Exact host match, never a suffix.** `allow "openai.com"` matching
/// `evil.openai.com.attacker.net` is the classic version of this bug, and
/// even a suffix rule that got it right would grant every subdomain the
/// provider owns. Nothing needs that, so nothing is granted it.
pub fn permitted(allow: &[String], host: &str, port: u16) -> bool {
    allow.iter().any(|entry| {
        // Rightmost colon, so a bracketed IPv6 literal keeps its own.
        match entry
            .rsplit_once(':')
            .and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h, p)))
        {
            Some((h, p)) => p == port && h.eq_ignore_ascii_case(host),
            None => port == 443 && entry.eq_ignore_ascii_case(host),
        }
    })
}

/// Parse a request line into a host and port.
///
/// Returns `None` for anything malformed, which the caller turns into a
/// refusal. Deliberately strict: an adapter is a program, not a person, and
/// there is no spelling of this line worth being lenient about.
pub fn parse_request(line: &str) -> Option<(String, u16)> {
    let rest = line
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("CONNECT ")?;
    let (host, port) = rest.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_owned(), port.parse().ok()?))
}

/// Start a broker whose socket lives in `dir`.
///
/// `allow` is Kobold's policy and never the adapter's: it is passed in from
/// the spawn site rather than read from anything the adapter controls, which
/// is the whole point. An **empty allowlist is a working broker that refuses
/// everything**, not a disabled one -- so a caller that forgets to grant a
/// host gets a refused connection rather than an open network.
pub fn start(allow: Vec<String>, dir: &Path) -> io::Result<Broker> {
    let path = dir.join("egress.sock");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    // Only this user. The socket is the adapter's whole network, so anything
    // that can open it can reach the allowlisted host through it.
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;

    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let allow = allow.clone();
            // One task per connection: a reconnect opens a new one while the
            // old one is still draining, and a broker that served them one at
            // a time would stall the reconnect it exists to permit.
            tokio::spawn(async move {
                let _ = serve(stream, &allow).await;
            });
        }
    });
    Ok(Broker { path, task })
}

/// One client: read its request, decide, and splice or refuse.
async fn serve(stream: UnixStream, allow: &[String]) -> io::Result<()> {
    let mut client = BufReader::new(stream);

    // Bounded before it is parsed, and read by hand rather than with
    // `read_line` for exactly that reason: `read_line` grows its `String`
    // until it finds a newline, so an adapter that opens the socket and sends
    // an endless stream of non-newline bytes would take Kobold's memory with
    // it. The longest legal request is a hostname and a port.
    //
    // **`take` rather than a length check in the loop**, which is a smaller
    // point than it looks. The check version compared `buf.len()` against the
    // cap after each chunk, and `>` versus `>=` there differ only at exactly
    // the cap -- where one refuses and the other waits forever. A test can
    // assert the refusal but can only *hang* on the other, which cargo-mutants
    // records as a timeout: not caught, not missed, green in the summary. With
    // the reader itself bounded there is no comparison left to get wrong.
    const MAX_REQUEST: u64 = 600;
    let mut buf = Vec::with_capacity(64);
    let read = {
        let mut limited = (&mut client).take(MAX_REQUEST);
        limited.read_until(b'\n', &mut buf).await?
    };
    if read == 0 {
        // Closed without asking for anything. Nothing to refuse.
        return Ok(());
    }
    if !buf.ends_with(b"\n") {
        // The cap was reached with no newline in sight, so there is no
        // request here and there never will be.
        client
            .get_mut()
            .write_all(b"DENY request too long\n")
            .await?;
        return Ok(());
    }
    let line = String::from_utf8_lossy(&buf).into_owned();

    let Some((host, port)) = parse_request(&line) else {
        client
            .get_mut()
            .write_all(b"DENY malformed request\n")
            .await?;
        return Ok(());
    };
    if !permitted(allow, &host, port) {
        // The reason names the host, because the adapter's author needs to
        // know which entry their manifest is missing. It says nothing about
        // what *is* allowed: an adapter that can enumerate the allowlist can
        // report it, and the allowlist is the user's configuration.
        client
            .get_mut()
            .write_all(format!("DENY {host}:{port} is not allowed\n").as_bytes())
            .await?;
        return Ok(());
    }

    // Resolved here rather than inside, which is what closes DNS as a covert
    // channel: the sandbox has no network, so it could not resolve anything
    // even if it wanted to.
    let upstream = match TcpStream::connect((host.as_str(), port)).await {
        Ok(s) => s,
        Err(e) => {
            client
                .get_mut()
                .write_all(format!("DENY {e}\n").as_bytes())
                .await?;
            return Ok(());
        }
    };
    let _ = upstream.set_nodelay(true);

    client.get_mut().write_all(OK.as_bytes()).await?;

    // Anything the client sent after its request line and before we answered
    // is already in the reader's buffer. Splicing the raw socket would drop
    // it, which is a torn TLS ClientHello and an error a long way from here.
    let buffered = client.buffer().to_vec();
    let mut client = client.into_inner();
    let (mut cr, mut cw) = client.split();
    let (mut ur, mut uw) = {
        let (r, w) = upstream.into_split();
        (r, w)
    };
    if !buffered.is_empty() {
        uw.write_all(&buffered).await?;
    }

    // Both directions until either end goes. `copy` returning is the peer
    // closing, and once one half is gone the other has nowhere to deliver.
    let up = async {
        let _ = tokio::io::copy(&mut cr, &mut uw).await;
        let _ = uw.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut ur, &mut cw).await;
        let _ = cw.shutdown().await;
    };
    tokio::join!(up, down);
    Ok(())
}
