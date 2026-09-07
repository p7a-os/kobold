//! What the egress broker permits and refuses, and that a confined adapter
//! can reach it and nothing else.
//!
//! **The end-to-end test runs a client inside the real sandbox**, which is
//! the only way to check the claim that matters: the adapter has no network
//! and the socket is nonetheless reachable. Asserting the argument list would
//! prove `--bind` was spelled correctly, which is not the same thing -- the
//! same mistake that let a read-only workdir bind look safe while `.env` was
//! readable through it.

use kobold::broker::{self, parse_request, permitted};

#[test]
fn only_an_allowlisted_host_on_443_is_permitted() {
    let allow = vec!["api.openai.com".to_owned()];
    assert!(permitted(&allow, "api.openai.com", 443));
    // Case, because DNS is case-insensitive and an adapter can spell it
    // however it likes.
    assert!(permitted(&allow, "API.OpenAI.COM", 443));

    // The suffix trap, both halves. A rule matching by suffix would let the
    // first through, and a rule matching "ends with the allowed name" would
    // let the second through -- and the second is a host the attacker owns.
    assert!(!permitted(&allow, "evil.api.openai.com", 443));
    assert!(!permitted(&allow, "api.openai.com.attacker.net", 443));
    assert!(!permitted(&allow, "notapi.openai.com", 443));

    // A bare entry grants 443 and nothing else, so a plaintext port is not
    // something a user can reach by accident.
    assert!(!permitted(&allow, "api.openai.com", 80));
    assert!(!permitted(&allow, "api.openai.com", 8443));

    // Naming a port grants exactly that port -- and, the half that stops this
    // being a way to widen the rule, still nothing else.
    let pinned = vec!["localhost:8443".to_owned()];
    assert!(permitted(&pinned, "localhost", 8443));
    assert!(
        !permitted(&pinned, "localhost", 443),
        "an explicit port must not also grant 443"
    );
    assert!(!permitted(&pinned, "elsewhere", 8443));

    // An empty allowlist is a broker that refuses everything, which is the
    // safe direction for a caller that forgot to grant a host.
    assert!(!permitted(&[], "api.openai.com", 443));
}

#[test]
fn a_request_line_is_parsed_strictly() {
    assert_eq!(
        parse_request("CONNECT api.openai.com:443\n"),
        Some(("api.openai.com".into(), 443))
    );
    assert_eq!(
        parse_request("CONNECT api.openai.com:443\r\n"),
        Some(("api.openai.com".into(), 443))
    );
    // Rightmost colon, so a bracketed IPv6 literal keeps its own colons.
    assert_eq!(
        parse_request("CONNECT [::1]:443\n"),
        Some(("[::1]".into(), 443))
    );

    for bad in [
        "GET / HTTP/1.1\n",
        "CONNECT\n",
        "CONNECT api.openai.com\n",
        "CONNECT api.openai.com:\n",
        "CONNECT api.openai.com:nope\n",
        "CONNECT :443\n",
        "connect api.openai.com:443\n",
        "",
    ] {
        assert_eq!(parse_request(bad), None, "accepted {bad:?}");
    }
}

/// Everything below needs a real socket pair, so they share a runtime helper.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// An echo server standing in for a provider, on loopback.
async fn echo_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    (addr, task)
}

/// Speak the broker's protocol over its socket and return the answer line
/// plus whatever came back after it.
async fn ask(path: &std::path::Path, request: &str, then: &[u8]) -> (String, Vec<u8>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let sock = tokio::net::UnixStream::connect(path)
        .await
        .expect("connect to broker");
    let mut sock = BufReader::new(sock);
    sock.get_mut()
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut answer = String::new();
    sock.read_line(&mut answer).await.expect("read answer");
    let mut body = Vec::new();
    if answer.trim_end() == "OK" && !then.is_empty() {
        sock.get_mut().write_all(then).await.expect("write payload");
        let mut got = vec![0u8; then.len()];
        sock.read_exact(&mut got).await.expect("read echo");
        body = got;
    }
    (answer.trim_end().to_owned(), body)
}

/// The permit path and the refuse path, in one test, against one broker.
///
/// Together rather than separately on purpose: a broker that refused
/// everything would pass the refusal test alone, and a broker that spliced
/// unconditionally would pass the permit test alone.
#[test]
fn the_broker_splices_an_allowed_host_and_refuses_the_rest() {
    rt().block_on(async {
        let (addr, _srv) = echo_server().await;
        let hutch = broker::hutch().expect("hutch");
        let b = broker::start(vec![format!("127.0.0.1:{}", addr.port())], hutch.path())
            .expect("broker");

        // Allowed: the bytes reach the server and come back, which is the
        // splice working in both directions.
        let (answer, echoed) = ask(
            b.path(),
            &format!("CONNECT 127.0.0.1:{}\n", addr.port()),
            b"hello through",
        )
        .await;
        assert_eq!(answer, "OK", "an allowlisted host was refused");
        assert_eq!(echoed, b"hello through", "the splice lost or altered bytes");

        // Not on the list. The reason names the host so an adapter author can
        // see which grant is missing.
        let (answer, _) = ask(b.path(), "CONNECT example.invalid:443\n", b"").await;
        assert!(
            answer.starts_with("DENY"),
            "an unlisted host was allowed: {answer}"
        );
        assert!(
            answer.contains("example.invalid"),
            "the refusal must name the host: {answer}"
        );

        // Malformed, which must be a refusal rather than a panic or a hang.
        let (answer, _) = ask(b.path(), "GET / HTTP/1.1\n", b"").await;
        assert!(answer.starts_with("DENY"), "junk was accepted: {answer}");
    });
}

/// A real client writes its first payload immediately after the request --
/// TLS sends its ClientHello without waiting -- so both arrive in one read
/// and the payload sits in the reader's buffer when the broker answers.
///
/// **Splicing the raw socket at that point drops it**, and the symptom is a
/// torn ClientHello reported as a TLS error a long way from here. The tests
/// above send the two halves in separate writes, which is the case that
/// happens not to exercise this.
#[test]
fn a_payload_sent_with_the_request_is_not_dropped_by_the_splice() {
    rt().block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let (addr, _srv) = echo_server().await;
        let hutch = broker::hutch().expect("hutch");
        let b = broker::start(vec![format!("127.0.0.1:{}", addr.port())], hutch.path())
            .expect("broker");

        let sock = tokio::net::UnixStream::connect(b.path())
            .await
            .expect("connect");
        let mut sock = BufReader::new(sock);
        // One write, request and payload together.
        let both = format!("CONNECT 127.0.0.1:{}\nearly bytes", addr.port());
        sock.get_mut()
            .write_all(both.as_bytes())
            .await
            .expect("write");

        let mut answer = String::new();
        sock.read_line(&mut answer).await.expect("answer");
        assert_eq!(
            answer.trim_end(),
            "OK",
            "the request half was not understood"
        );

        // **Bounded, because the failure this guards is a read that never
        // returns.** Dropped bytes are bytes the echo server never sees, so
        // it never replies -- and an unbounded `read_exact` would hang rather
        // than fail, which cargo-mutants records as a timeout: not caught,
        // not missed, green in the summary. The bound turns the corpse into
        // an assertion. Loopback with the server already accepted, so seconds
        // are orders of magnitude of headroom.
        let mut echoed = vec![0u8; b"early bytes".len()];
        let got = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            sock.read_exact(&mut echoed),
        )
        .await;
        assert!(
            got.is_ok(),
            "the early bytes never came back: they were dropped by the splice"
        );
        got.expect("bounded above").expect("read");
        assert_eq!(
            &echoed, b"early bytes",
            "bytes buffered alongside the request were altered by the splice"
        );
    });
}

/// A client that never sends a newline must not be able to grow Kobold's
/// memory. It gets refused instead, at a bound.
#[test]
fn an_endless_request_line_is_refused_rather_than_buffered() {
    rt().block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let hutch = broker::hutch().expect("hutch");
        let b = broker::start(vec!["127.0.0.1:443".to_owned()], hutch.path()).expect("broker");
        let sock = tokio::net::UnixStream::connect(b.path())
            .await
            .expect("connect");
        let mut sock = BufReader::new(sock);
        // Well past the bound, no newline anywhere.
        let flood = vec![b'A'; 4096];
        // The write may fail once the broker has answered and gone, which is
        // the behaviour under test rather than a problem.
        let _ = sock.get_mut().write_all(&flood).await;

        // **The test finishes on a byte count it controls, not on the bound
        // it is testing.** Without this shutdown the broker keeps reading and
        // the client keeps waiting, so breaking the comparison made this
        // *hang* rather than fail -- two of the three mutants of that guard
        // were timeouts, not misses, and a test process dying by signal reads
        // as flaky infrastructure rather than as a defect being named.
        //
        // Closing our write half means the broker sees EOF at a point we
        // chose. If it never refused, it returns and the socket closes, and
        // `read_line` gives back an empty string -- a failed assertion with a
        // message, in bounded time.
        let _ = sock.get_mut().shutdown().await;

        let mut answer = String::new();
        sock.read_line(&mut answer).await.expect("read");
        assert!(
            answer.starts_with("DENY"),
            "an unbounded request was buffered rather than refused (answer: {answer:?})"
        );
    });
}

/// The other side of the bound, and the mutant it kills.
///
/// `> MAX_REQUEST` inverted to `<` refuses everything, including a legal
/// request -- which the flood test above cannot see, because a flood is
/// refused either way. This one is refused for the *right reason*: the host
/// is not on the allowlist, which means the length guard let it through.
#[test]
fn a_request_well_inside_the_bound_is_judged_on_its_host_and_not_its_length() {
    rt().block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let hutch = broker::hutch().expect("hutch");
        let b = broker::start(vec!["allowed.example".to_owned()], hutch.path()).expect("broker");
        let sock = tokio::net::UnixStream::connect(b.path())
            .await
            .expect("connect");
        let mut sock = BufReader::new(sock);

        // Long enough that a bound set anywhere sane would have to have
        // decided, short enough to be legal. 500-odd bytes against 600.
        let host = format!("{}.example", "a".repeat(480));
        sock.get_mut()
            .write_all(format!("CONNECT {host}:443\n").as_bytes())
            .await
            .expect("write");
        let mut answer = String::new();
        sock.read_line(&mut answer).await.expect("read");
        assert!(
            answer.contains("is not allowed"),
            "a legal-length request was refused for its length: {answer:?}"
        );
        assert!(
            !answer.contains("too long"),
            "the length guard fired on a request inside the bound: {answer:?}"
        );
    });
}

/// **Both teardowns, because a `Drop` that does nothing is a live egress path
/// after the adapter is gone.** Neither had a test: emptying either one
/// survived mutation, and what survives there is a socket nobody is watching
/// still able to reach the allowlisted host.
#[test]
fn dropping_the_hutch_takes_the_directory_and_the_socket_with_it() {
    // Inside a runtime because `start` spawns the accept task; the hutch
    // itself needs none.
    rt().block_on(async {
        let path;
        {
            let hutch = broker::hutch().expect("hutch");
            path = hutch.path().to_path_buf();
            let b =
                broker::start(vec!["allowed.example".to_owned()], hutch.path()).expect("broker");
            // The positive half: it really was there, so the absence below is
            // a removal rather than a directory that never existed.
            assert!(path.is_dir(), "the hutch was not created");
            assert!(b.path().exists(), "the socket was not created");
        }
        assert!(
            !path.exists(),
            "the hutch outlived the adapter that owned it"
        );
    });
}

#[test]
fn dropping_the_broker_removes_the_socket_and_stops_answering() {
    rt().block_on(async {
        let hutch = broker::hutch().expect("hutch");
        let sock_path;
        {
            let b =
                broker::start(vec!["allowed.example".to_owned()], hutch.path()).expect("broker");
            sock_path = b.path().to_path_buf();
            // Positive half first. Without it, "connect fails after drop"
            // is satisfied by a broker that never accepted anything.
            let live = tokio::net::UnixStream::connect(&sock_path).await;
            assert!(
                live.is_ok(),
                "the broker never accepted a connection: {live:?}"
            );
        }
        assert!(!sock_path.exists(), "the socket outlived the broker");
        // And the listener is gone rather than merely unnamed: a socket file
        // removed while the listener still holds the bound fd would keep
        // serving anyone who already had a connection open.
        assert!(
            tokio::net::UnixStream::connect(&sock_path).await.is_err(),
            "the broker still answers after being dropped"
        );
    });
}

/// The claim the whole design rests on: **inside the sandbox there is no
/// network, and the broker's socket is still reachable.**
///
/// Skipped rather than failed where `bwrap` is absent -- there is no sandbox
/// to make claims about, and pretending otherwise would be a green run that
/// measured nothing.
#[test]
#[cfg(target_os = "linux")]
fn a_confined_client_has_no_network_but_can_still_reach_the_broker() {
    if kobold::sandbox::available().is_none() {
        return;
    }
    let Some(python) = kobold::sandbox::resolve("python3") else {
        // No interpreter inside means no way to speak a unix socket from a
        // shell, so there is nothing to assert. Saying so beats a green skip.
        eprintln!("python3 not found; the confined-client check did not run");
        return;
    };

    rt().block_on(async {
        let (addr, _srv) = echo_server().await;
        let hutch = broker::hutch().expect("hutch");
        let b = broker::start(vec![format!("127.0.0.1:{}", addr.port())], hutch.path())
            .expect("broker");

        let script = format!(
            "import socket,sys\n\
             try:\n\
             \x20 t=socket.create_connection(('127.0.0.1',{port}),2); print('DIRECT_OK')\n\
             except Exception as e:\n\
             \x20 print('direct_blocked')\n\
             s=socket.socket(socket.AF_UNIX); s.connect('{sock}')\n\
             s.sendall(b'CONNECT 127.0.0.1:{port}\\n')\n\
             hdr=b''\n\
             while not hdr.endswith(b'\\n'): hdr += s.recv(1)\n\
             print('broker:'+hdr.decode().strip())\n\
             s.sendall(b'ping'); print('echo:'+s.recv(4).decode())\n",
            port = addr.port(),
            sock = b.path().display(),
        );

        let argv = vec![
            python.to_string_lossy().into_owned(),
            "-c".to_owned(),
            script,
        ];
        let policy = kobold::sandbox::Policy {
            allow_network: false,
            writable_root: false,
            share_workdir: false,
        };
        let cwd = std::env::current_dir().expect("cwd");
        let mut cmd = kobold::sandbox::wrap_with_socket(policy, &cwd, &argv, b.path())
            .expect("bwrap is present");
        let out = cmd.output().await.expect("the confined client ran");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

        assert!(
            stdout.contains("direct_blocked"),
            "the confined client reached the network directly: {stdout}{stderr}"
        );
        assert!(
            stdout.contains("broker:OK"),
            "the confined client could not reach the broker: {stdout}{stderr}"
        );
        assert!(
            stdout.contains("echo:ping"),
            "the splice did not carry bytes into the sandbox: {stdout}{stderr}"
        );
    });
}
