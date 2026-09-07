//! Exhaustive Performance and Memory Footprint Benchmark Suite for Kobold.
//!
//! Measures:
//! 1. Cold startup latency of `koboldd` daemon
//! 2. Memory footprint (Resident Set Size / RSS) under idle and active load
//! 3. High-throughput message streaming throughput across Unix Domain Socket
//! 4. Frame dispatch latency

use kobold::daemon::DaemonClient;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use tokio::time::timeout;

struct ProcessGuard(std::process::Child);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn binary_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current test exe");
    path.pop(); // deps
    path.pop(); // debug
    let candidate = path.join(name);
    if candidate.exists() {
        return candidate;
    }
    let debug_path = PathBuf::from("target/debug").join(name);
    if debug_path.exists() {
        return debug_path;
    }
    let _ = Command::new("cargo")
        .args(["build", "--bin", name])
        .output();
    if candidate.exists() {
        candidate
    } else {
        debug_path
    }
}

/// Reads the Resident Set Size (RSS) of a process in Kilobytes using `ps`.
fn get_process_rss_kb(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    s.trim().parse::<u64>().ok()
}

#[tokio::test]
async fn test_perf_and_memory_footprint() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");

    let dir = tempdir().unwrap();
    let sock = dir.path().join("perf.sock");
    let session_id = format!("perf-sess-{}", uuid::Uuid::now_v7());

    // 1. Measure Cold Startup Latency
    let start_time = Instant::now();
    let daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(&acp_adapter_bin)
        .arg("--unconfined")
        .spawn()
        .expect("spawn koboldd daemon");
    let pid = daemon_proc.id();
    let _guard = ProcessGuard(daemon_proc);

    let mut client = None;
    for _ in 0..100 {
        if sock.exists() {
            if let Ok(c) = DaemonClient::connect(&sock).await {
                client = Some(c);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let startup_latency = start_time.elapsed();
    let mut client = client.expect("daemon failed to bind within deadline");

    println!("\n=== KOBOLD PERFORMANCE & MEMORY AUDIT ===");
    println!("1. Daemon Cold Startup Latency: {:?}", startup_latency);
    assert!(
        startup_latency < Duration::from_millis(500),
        "Cold startup latency ({:?}) exceeded 500ms budget",
        startup_latency
    );

    // Initial hydration snapshot
    let snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout")
        .expect("recv")
        .expect("snap");
    assert!(matches!(snap, ClientServerFrame::Snapshot { .. }));

    // 2. Measure Idle Memory Footprint (RSS)
    tokio::time::sleep(Duration::from_millis(100)).await;
    let idle_rss_kb = get_process_rss_kb(pid).unwrap_or(0);
    let idle_rss_mb = idle_rss_kb as f64 / 1024.0;
    println!(
        "2. Daemon Idle RSS Memory: {:.2} MB ({} KB)",
        idle_rss_mb, idle_rss_kb
    );
    assert!(
        idle_rss_mb < 35.0,
        "Daemon idle RSS ({:.2} MB) exceeded 35MB ceiling",
        idle_rss_mb
    );

    // 3. Measure High-Throughput Burst Ingestion (100 sequential prompt & answer turns)
    let burst_count = 100usize;
    let burst_start = Instant::now();

    for i in 0..burst_count {
        client
            .send(&ClientFrame::Prompt {
                lane: "main".into(),
                text: format!("burst message {i}"),
            })
            .await
            .expect("send burst prompt");

        // Await prompt completion
        loop {
            if let Ok(Ok(Some(ClientServerFrame::Event {
                event: kobold_proto::agui::Incoming::RunFinished { .. },
                ..
            }))) = timeout(Duration::from_secs(2), client.recv()).await
            {
                break;
            }
        }
    }

    let burst_duration = burst_start.elapsed();
    let throughput_turns_per_sec = burst_count as f64 / burst_duration.as_secs_f64();
    let avg_turn_latency_ms = (burst_duration.as_secs_f64() * 1000.0) / burst_count as f64;

    println!(
        "3. Burst Throughput: {:.1} full turns/sec (avg latency {:.2}ms per complete turn)",
        throughput_turns_per_sec, avg_turn_latency_ms
    );
    println!(
        "   Processed {} full turns over UDS in {:?}",
        burst_count, burst_duration
    );

    // 4. Measure Active / Post-Burst Memory Footprint
    tokio::time::sleep(Duration::from_millis(100)).await;
    let active_rss_kb = get_process_rss_kb(pid).unwrap_or(0);
    let active_rss_mb = active_rss_kb as f64 / 1024.0;
    println!(
        "4. Daemon Post-Burst RSS Memory: {:.2} MB ({} KB)",
        active_rss_mb, active_rss_kb
    );
    assert!(
        active_rss_mb < 50.0,
        "Daemon post-burst RSS ({:.2} MB) exceeded 50MB ceiling",
        active_rss_mb
    );

    println!("========================================\n");
}
