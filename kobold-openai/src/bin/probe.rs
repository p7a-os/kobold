//! Measurement tool, not part of the client.
//!
//!   probe ext                 -> what extensions the server negotiates
//!   probe traffic "<prompt>"  -> dump every event payload for sizing

use kobold_openai::{events, json, ws};

use std::io::Write;

// `current_thread`, matching the adapter's own `main`: this crate
// deliberately does not depend on tokio's multi-threaded runtime, because a
// provider adapter is meant to be small and confinable. A bare
// `#[tokio::main]` asks for `rt-multi-thread` and only compiled because the
// workspace build unified features with kobold's tokio -- `cargo build -p
// kobold-openai` on its own did not.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("LLM_API_KEY").or_else(|_| std::env::var("OPENAI_API_KEY"))?;
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ext".into());

    if mode == "ext" {
        let (_ws, res) =
            ws::connect_inspect(&key, Some("permessage-deflate; client_max_window_bits")).await?;
        println!("status: {}", res.status());
        if let Some(v) = res.headers().get("sec-websocket-extensions") {
            println!("negotiated: {}", v.to_str()?);
        } else {
            println!("negotiated: <none>");
        }
        return Ok(());
    }

    if mode == "timing" {
        // Phase-by-phase connection cost. Run twice in one process so the
        // second pass shows what a reconnect costs with the TLS config cached
        // and a session ticket in hand.
        for pass in 1..=2 {
            let t0 = std::time::Instant::now();
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((ws::HOSTNAME, 443))
                .await?
                .collect();
            let dns = t0.elapsed();

            let t1 = std::time::Instant::now();
            let tcp = tokio::net::TcpStream::connect(addrs[0]).await?;
            tcp.set_nodelay(true)?;
            let connect = t1.elapsed();

            // TLS handshake alone, on the socket we just opened.
            let t2 = std::time::Instant::now();
            let _tls = ws::tls_handshake(tcp).await?;
            let tls = t2.elapsed();

            // Whole thing again, so upgrade cost = full - (tcp + tls).
            let t3 = std::time::Instant::now();
            let (_ws, _res) = ws::connect_inspect(&key, None).await?;
            let full = t3.elapsed();

            println!(
                "pass {pass}: dns {:>8.2?} | tcp {:>8.2?} | tls {:>8.2?} | upgrade {:>8.2?} | total {:>8.2?} | peer {}",
                dns,
                connect,
                tls,
                full.saturating_sub(connect + tls),
                full,
                addrs[0]
            );
        }
        return Ok(());
    }

    if mode == "turn" {
        // Per-turn latency: send -> first delta byte. Run under KOBOLD_NAGLE=1
        // to A/B TCP_NODELAY.
        let mut conn = ws::connect(&key).await?;
        let mut samples = Vec::new();
        let mut prev: Option<String> = None;
        for _ in 0..5 {
            let mut create =
                events::ResponseCreate::user_text("gpt-5.6-luna", Some("main"), "Say ok.", "none");
            create.previous_response_id = prev.as_deref();
            let body = json::to_string(&create)?;
            let t = std::time::Instant::now();
            ws::send_text(&mut conn, &body).await?;
            let mut first: Option<std::time::Duration> = None;
            loop {
                let msg = match ws::read(&mut conn).await? {
                    ws::Incoming::Text(b) => b,
                    _ => break,
                };
                let ev: events::Event = match json::from_slice(msg.as_ref()) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if first.is_none() && ev.kind == "response.output_text.delta" {
                    first = Some(t.elapsed());
                }
                if ev.is_terminal() {
                    prev = ev.response.as_ref().map(|r| r.id.to_owned());
                    break;
                }
            }
            if let Some(d) = first {
                samples.push(d);
            }
        }
        samples.sort();
        println!(
            "nagle={} first-delta: min {:.1?} median {:.1?} max {:.1?} (n={})",
            std::env::var_os("KOBOLD_NAGLE").is_some(),
            samples.first().unwrap(),
            samples[samples.len() / 2],
            samples.last().unwrap(),
            samples.len()
        );
        return Ok(());
    }

    if mode == "cancel" {
        // Is there a client-side cancel in WebSocket mode? The guide does not
        // document one, so ask the server directly.
        let mut conn = ws::connect(&key).await?;
        let create = events::ResponseCreate::user_text(
            "gpt-5.6-luna",
            Some("main"),
            "Count slowly from 1 to 200, one number per line.",
            "none",
        );
        ws::send_text(&mut conn, &json::to_string(&create)?).await?;

        let mut sent_cancel = false;
        let mut deltas_after_cancel = 0;
        loop {
            let msg = match ws::read(&mut conn).await? {
                ws::Incoming::Text(b) => b,
                ws::Incoming::Closed => {
                    println!("server closed");
                    break;
                }
                ws::Incoming::Other => continue,
            };
            let raw = String::from_utf8_lossy(msg.as_ref()).to_string();
            let ev: events::Event = match json::from_slice(msg.as_ref()) {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !sent_cancel && ev.kind == "response.output_text.delta" {
                sent_cancel = true;
                println!("-> sending response.cancel");
                ws::send_text(
                    &mut conn,
                    "{\"type\":\"response.cancel\",\"stream_id\":\"main\"}",
                )
                .await?;
                continue;
            }
            if sent_cancel {
                if ev.kind == "response.output_text.delta" {
                    deltas_after_cancel += 1;
                } else {
                    println!("<- {}", &raw[..raw.len().min(240)]);
                }
            }
            if ev.is_terminal() {
                println!(
                    "terminal: {} | deltas after cancel: {}",
                    ev.kind, deltas_after_cancel
                );
                break;
            }
        }
        return Ok(());
    }

    let prompt = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "Explain TCP congestion control.".into());
    let mut conn = ws::connect(&key).await?;
    let create = events::ResponseCreate::user_text("gpt-5.6-luna", Some("main"), &prompt, "none");
    let out_bytes = json::to_string(&create)?;
    ws::send_text(&mut conn, &out_bytes).await?;

    let mut dump = std::fs::File::create("traffic.bin")?;
    let (mut msgs, mut bytes) = (0usize, 0usize);

    loop {
        let msg = match ws::read(&mut conn).await? {
            ws::Incoming::Text(b) => b,
            _ => break,
        };
        msgs += 1;
        bytes += msg.len();
        dump.write_all(msg.as_ref())?;
        let ev: events::Event = match json::from_slice(msg.as_ref()) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if ev.is_terminal() {
            break;
        }
    }

    println!("sent    : {} bytes", out_bytes.len());
    println!("received: {msgs} messages, {bytes} bytes");
    println!("mean msg: {} bytes", bytes / msgs.max(1));
    Ok(())
}
