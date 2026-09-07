# Performance & Memory Footprint Audit

Kobold is built to be blazing fast with a near-zero memory footprint. Every release is verified against an exhaustive performance and memory benchmark suite.

---

## 1. Daemon & IPC Performance Audit

Results measured under automated release audit (`tests/perf_audit.rs`):

| Metric | Measured Value | Budget Ceiling | Margin |
| :--- | :--- | :--- | :--- |
| **Daemon Cold Startup** | **~209 ms** | < 500 ms | **2.4x faster** |
| **Idle Memory Footprint (RSS)** | **6.11 MB** | < 35 MB | **5.7x leaner** |
| **Post-Burst Memory Footprint (100 turns)** | **7.75 MB** | < 50 MB | **6.4x leaner** |
| **IPC Throughput** | **2,087 full turns/sec** | > 500 turns/sec | **4.1x faster** |
| **Average Turn Roundtrip Latency** | **0.48 ms** | < 5.0 ms | **10.4x faster** |

*Note: Measured on Apple Silicon aarch64 under native release optimizations.*

---

## 2. Terminal UI Render Latency

Results measured via `cargo run --release --bin bench`:

```
terminal 100x30

stream-prose           n=487   mean    20.6µs  p50    17.3µs  p99   151.2µs  growth  1.00x
stream-full            n=1248  mean    22.1µs  p50    18.2µs  p99    91.8µs  growth  0.38x
stream-800-line-fence  n=3008  mean    21.4µs  p50    18.5µs  p99    58.5µs  growth  1.26x
keystroke-echo         n=43    mean    18.1µs  p50    17.9µs  p99    25.3µs  growth  1.00x

frame-vs-history           1 turns   11.6µs    10 turns   11.7µs    50 turns   12.1µs   200 turns   13.0µs
wire-cost              scroll-hint     93 B/frame    39.9µs   repaint    590 B/frame   34.0µs   6.3x fewer bytes
                       97.5% of cells elided, 40 scrolls, 1 full repaints over 41 frames
```

---

## 3. Key Architectural Optimizations

1. **Differential Terminal Painting with Scroll-Hints**:
   Kobold tracks terminal scroll offsets and emits hardware scroll escape sequences rather than re-rendering the whole screen on every line wrap. This achieves a **6.3x reduction in wire bytes** and elides **97.5% of buffer cell paints**.
2. **Flat $O(1)$ History Scaling**:
   Laying out an hour-long session with 200 turns costs **13.0 µs**, almost identical to a fresh 1-turn session (**11.6 µs**). History off-screen does not add per-frame computational overhead.
3. **Zero Allocations on Steady-State Streams**:
   Buffers are retained across frame render ticks. Cell attributes are bit-packed, minimizing cache thrashing and GC pauses.
4. **Sub-8MB Resident Set Size**:
   The headless supervisor kernel uses slab allocators, zero-copy string references where possible, and compact representation for the transcript DAG.
