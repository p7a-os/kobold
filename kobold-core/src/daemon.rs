//! Daemon supervisor and UDS server for Kobold.
//!
//! Hosts the headless [`Kernel`], ingesting [`ClientFrame`] commands from
//! multiple attached frontends over Unix Domain Sockets, managing adapter
//! subprocesses and MCP tools, and broadcasting [`ServerFrame`] events to
//! all connected clients.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};

use kobold_proto::codec;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame, ServerFrame};
use kobold_proto::{Command, IncomingFrame};

use crate::kernel::Kernel;

/// Default socket path format for session daemons: `/tmp/kobold-{uid}/{session_id}.sock`.
pub fn default_socket_path(session_id: &str) -> PathBuf {
    crate::session::session_runtime_dir().join(format!("{session_id}.sock"))
}

/// Configuration for running a Kobold daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
    pub root: PathBuf,
    pub initial_lane: String,
    pub initial_branch: String,
    pub ws_port: Option<u16>,
    pub ws_token: Option<String>,
}

impl DaemonConfig {
    pub fn new(socket_path: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            root: root.into(),
            initial_lane: "main".into(),
            initial_branch: "main".into(),
            ws_port: None,
            ws_token: None,
        }
    }

    pub fn with_ws(mut self, port: u16, token: impl Into<String>) -> Self {
        self.ws_port = Some(port);
        self.ws_token = Some(token.into());
        self
    }
}

/// Internal messages routed to the daemon actor loop.
pub(crate) enum DaemonMsg {
    ClientFrame {
        client_id: usize,
        frame: ClientFrame,
    },
    NewClient {
        client_id: usize,
        reply: mpsc::Sender<ServerFrame>,
    },
    ClientDisconnected(usize),
}

/// The Kobold headless daemon server.
pub struct Daemon {
    config: DaemonConfig,
    listener: UnixListener,
}

impl Daemon {
    /// Binds the daemon to the specified Unix Domain Socket.
    /// Cleans up any stale existing socket file before binding.
    pub fn bind(config: DaemonConfig) -> Result<Self, io::Error> {
        if let Some(parent) = config.socket_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if config.socket_path.exists() {
            let _ = fs::remove_file(&config.socket_path);
        }

        let listener = UnixListener::bind(&config.socket_path)?;
        Ok(Self { config, listener })
    }

    /// Returns the socket path this daemon is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.config.socket_path
    }

    /// Runs the daemon server event loop until `shutdown_rx` fires.
    pub async fn run(
        self,
        cmd_tx: mpsc::UnboundedSender<Command>,
        mut adapter_rx: mpsc::Receiver<IncomingFrame>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<(), io::Error> {
        let (broadcast_tx, _) = broadcast::channel::<ServerFrame>(256);
        let (msg_tx, mut msg_rx) = mpsc::channel::<DaemonMsg>(128);

        let mut kernel = Kernel::new(
            &self.config.initial_lane,
            &self.config.initial_branch,
            &self.config.root,
        );

        let listener = self.listener;
        let socket_path = self.config.socket_path.clone();

        // Spawn optional Northbound WebSocket companion server
        if let (Some(port), Some(token)) = (self.config.ws_port, self.config.ws_token.as_ref()) {
            let tcp_listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
            let ws_token = std::sync::Arc::new(token.clone());
            let ws_msg_tx = msg_tx.clone();
            let ws_broadcast_tx = broadcast_tx.clone();
            let ws_shutdown_rx = shutdown_rx.clone();

            tokio::spawn(async move {
                let _ = crate::ws::run_ws_server(
                    tcp_listener,
                    ws_token,
                    ws_msg_tx,
                    ws_broadcast_tx,
                    ws_shutdown_rx,
                )
                .await;
            });
        }

        // Spawn client accept loop
        let accept_msg_tx = msg_tx.clone();
        let accept_broadcast_tx = broadcast_tx.clone();
        let accept_handle = tokio::spawn(async move {
            let mut next_client_id = 1usize;
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(_) => break,
                };

                let client_id = next_client_id;
                next_client_id = next_client_id.wrapping_add(1);

                let (read_half, mut write_half) = stream.into_split();
                let (initial_tx, mut initial_rx) = mpsc::channel::<ServerFrame>(16);
                let mut client_sub = accept_broadcast_tx.subscribe();

                // Notify daemon loop of new client connection to hydrate initial state
                let _ = accept_msg_tx
                    .send(DaemonMsg::NewClient {
                        client_id,
                        reply: initial_tx,
                    })
                    .await;

                // Writer task for this client
                let writer_handle = tokio::spawn(async move {
                    // First drain any initial hydration frames
                    while let Some(frame) = initial_rx.recv().await {
                        if let Ok(line) = codec::encode(&frame) {
                            if write_half.write_all(line.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                    }

                    // Then stream broadcast events
                    while let Ok(frame) = client_sub.recv().await {
                        if let Ok(line) = codec::encode(&frame) {
                            if write_half.write_all(line.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                    }
                });

                // Reader task for this client
                let reader_msg_tx = accept_msg_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(read_half);
                    let mut line_buf = String::new();

                    loop {
                        line_buf.clear();
                        match reader.read_line(&mut line_buf).await {
                            Ok(0) => break, // EOF: client disconnected
                            Ok(_) => {
                                if let Ok(frame) = codec::decode::<ClientFrame>(&line_buf) {
                                    let is_detach = matches!(frame, ClientFrame::Detach);
                                    let _ = reader_msg_tx
                                        .send(DaemonMsg::ClientFrame { client_id, frame })
                                        .await;
                                    if is_detach {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    let _ = reader_msg_tx
                        .send(DaemonMsg::ClientDisconnected(client_id))
                        .await;
                    writer_handle.abort();
                });
            }
        });

        let mut active_clients: Vec<usize> = Vec::new();
        let mut rw_client_id: Option<usize> = None;

        // Main daemon processing loop
        let result = loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break Ok(());
                    }
                }
                Some(frame) = adapter_rx.recv() => {
                    let outbound = kernel.handle_adapter_frame(frame, &cmd_tx).await;
                    for f in outbound {
                        let _ = broadcast_tx.send(f);
                    }
                }
                Some(msg) = msg_rx.recv() => {
                    match msg {
                        DaemonMsg::NewClient { client_id, reply } => {
                            // Hydrate the newly connected client with snapshots of all active lanes
                            for lane in &kernel.lanes {
                                let snapshot = ServerFrame::Snapshot {
                                    lane: lane.name.clone(),
                                    branch: lane.branch.clone(),
                                    messages: lane.snapshot_records(),
                                    active_interrupt: lane.active_ask_record(),
                                    status: lane.status(),
                                };
                                let _ = reply.send(snapshot).await;
                            }

                            active_clients.push(client_id);
                            if rw_client_id.is_none() {
                                rw_client_id = Some(client_id);
                            } else {
                                let _ = reply.send(ServerFrame::Notice {
                                    text: "Attached to session in read-only mode (another client is active)".into(),
                                }).await;
                            }
                        }
                        DaemonMsg::ClientFrame { client_id, frame } => {
                            if rw_client_id == Some(client_id) {
                                let outbound = kernel.handle_client_frame(frame, &cmd_tx).await;
                                for f in outbound {
                                    let _ = broadcast_tx.send(f);
                                }
                            } else if matches!(frame, ClientFrame::Detach) {
                                // Allow read-only clients to detach cleanly
                            } else {
                                let notice = ServerFrame::Notice {
                                    text: "Command ignored: client is attached in read-only mode".into(),
                                };
                                let _ = broadcast_tx.send(notice);
                            }
                        }
                        DaemonMsg::ClientDisconnected(client_id) => {
                            active_clients.retain(|&id| id != client_id);
                            if rw_client_id == Some(client_id) {
                                rw_client_id = active_clients.first().copied();
                                if rw_client_id.is_some() {
                                    let _ = broadcast_tx.send(ServerFrame::Notice {
                                        text: "Previous read-write client disconnected; client promoted to read-write mode".into(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        };

        accept_handle.abort();
        let _ = fs::remove_file(&socket_path);
        result
    }
}

/// Client handle for communicating with a running `koboldd` daemon.
pub struct DaemonClient {
    write_half: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    line_buf: String,
}

impl DaemonClient {
    /// Connects to a daemon at the given socket path.
    pub async fn connect(socket_path: impl AsRef<Path>) -> Result<Self, io::Error> {
        let stream = UnixStream::connect(socket_path).await?;
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            write_half,
            reader: BufReader::new(read_half),
            line_buf: String::new(),
        })
    }

    /// Sends a [`ClientFrame`] to the daemon.
    pub async fn send(&mut self, frame: &ClientFrame) -> Result<(), io::Error> {
        let line =
            codec::encode(frame).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.write_half.write_all(line.as_bytes()).await?;
        self.write_half.flush().await?;
        Ok(())
    }

    /// Reads the next [`ClientServerFrame`] from the daemon. Returns `None` on EOF.
    pub async fn recv(&mut self) -> Result<Option<ClientServerFrame>, io::Error> {
        self.line_buf.clear();
        let n = self.reader.read_line(&mut self.line_buf).await?;
        if n == 0 {
            return Ok(None);
        }
        let frame = codec::decode::<ClientServerFrame>(&self.line_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(frame))
    }

    /// Splits the client connection into an asynchronous writer channel
    /// and reader channel suitable for driving decoupled UI event loops.
    pub fn into_channels(
        mut self,
    ) -> (
        mpsc::UnboundedSender<ClientFrame>,
        mpsc::Receiver<ClientServerFrame>,
    ) {
        let (tx_out, mut rx_out) = mpsc::unbounded_channel::<ClientFrame>();
        let (tx_in, rx_in) = mpsc::channel::<ClientServerFrame>(128);

        // Writer task
        tokio::spawn(async move {
            while let Some(frame) = rx_out.recv().await {
                if let Ok(line) = codec::encode(&frame) {
                    if self.write_half.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    if self.write_half.flush().await.is_err() {
                        break;
                    }
                }
            }
        });

        // Reader task
        tokio::spawn(async move {
            loop {
                self.line_buf.clear();
                match self.reader.read_line(&mut self.line_buf).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        if let Ok(frame) = codec::decode::<ClientServerFrame>(&self.line_buf) {
                            if tx_in.send(frame).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        (tx_out, rx_in)
    }
}
