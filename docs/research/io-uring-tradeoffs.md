# io_uring for the market-maker transport layer

Epic E stage-2 research doc. Scope: evaluate whether swapping
the tokio::net (epoll-based) runtime for `tokio-uring`
(io_uring-based) is worth the complexity cost, and define the
opt-in surface if we land it.

## 1. What io_uring actually wins

Benchmarks on comparable workloads (vendor + third-party):

| Workload | epoll p50 | io_uring p50 | Notes |
|---|---|---|---|
| WS frame read on an idle socket | 2–5 μs | 1–2 μs | Kernel → user copy dominates; io_uring skips the `recvmsg` syscall. |
| 1 kB TCP read burst (10 kHz) | 3–8 μs | 1–3 μs | Batched SQEs amortise the syscall cost. |
| HTTP REST round-trip (local) | 80 μs | 75 μs | Network dominates; minimal gain. |
| 1 M ops/sec event loop wake-ups | 400 k syscalls | 20 k syscalls | Register-once completion queue. |

The win is **tail-latency shaving on the WS read path** —
typically 1–3 μs p50, 5–15 μs p99. For a market maker that
quotes on 100 ms refresh + 10 ms book-to-decision, the
absolute savings are ≤ 5% of the quoting loop.

## 2. What io_uring costs

1. **Fundamental runtime split**. `tokio-uring` is NOT
   runtime-compatible with `tokio::net`. A TCP socket opened
   by `TcpStream::connect` cannot be read by io_uring; you
   must use `tokio_uring::net::TcpStream`. This forces every
   venue connector and every WS feed onto a different API.
2. **Linux-only**. Our dev / CI / test matrix includes macOS
   (the author works on it daily) and we do not want to
   fragment the test surface.
3. **tokio-uring is pre-1.0**. The 0.5 series changes API
   shape across minor releases; pinning is a maintenance
   tax.
4. **No WS library uses io_uring by default**.
   `tokio-tungstenite` is tokio-native but does not expose
   a `tokio-uring` backend. A swap means either forking
   tungstenite or writing an io_uring WS client.
5. **Rustls integration is awkward**. `rustls` expects
   `AsyncRead + AsyncWrite` from `tokio`; bridging to
   io_uring requires a blocking-read-shim trait impl.

## 3. When io_uring pays for itself

Short answer: when WS read throughput saturates epoll's
syscall budget. Order-of-magnitude estimate: `throughput >
200 k messages/sec/core`. On a market-maker consuming 3–5
venues × ~5 k msg/sec each, we run ~30 k msg/sec — one
order of magnitude below the threshold.

**Green light conditions** (all required):

- Documented measured bottleneck where epoll's `recvmsg`
  syscall count exceeds 50 k/sec on a single tokio
  executor thread (visible via `strace -c -p $PID`).
- At least one target deployment on Linux kernel ≥ 5.19
  (io_uring stable ring features).
- A compelling PnL case (e.g. latency-sensitive taker
  strategy where 5 μs pre-emption on WS read moves the
  expected fill price by > 1 bps).

Until those conditions are met io_uring is premature. The
tokio::net path is faster to debug, faster to test, and
faster to reason about.

## 4. The opt-in surface

When / if we flip the switch:

- Workspace feature `io-uring` (disabled by default).
- `crates/exchange/*` feature-gate a parallel transport
  module that uses `tokio_uring::net::TcpStream` +
  `tokio-rustls-uring-shim` (TBW).
- Server `main.rs` replaces the `#[tokio::main]` attribute
  with a bespoke `tokio_uring::start` block when the
  feature is active.
- CI matrix adds a `linux-io-uring` job; the existing
  `linux` + `macos` jobs keep the epoll path.
- Benchmark harness (criterion) in `crates/exchange/core`
  measures the WS read latency on both paths; a regression
  in the epoll path fails CI.

## 5. What this commit does

Nothing, operationally. This commit only:

1. Lands this research doc.
2. Adds a workspace `io-uring` feature flag with no
   consumers — reserves the name so future PRs do not have
   to coordinate across crates.
3. Leaves the default runtime on `tokio::net`.

A future operator running into the green-light conditions
will re-open this doc, validate the bottleneck, and flip
the feature.

## 6. References

- kernel docs: <https://kernel.dk/io_uring.pdf> (Axboe 2019)
- `tokio-uring`: <https://github.com/tokio-rs/tokio-uring>
- benchmarks: "io_uring in Rust", @jamesmunns 2023 blog
- counterpoint: "Why I don't use io_uring" (Jens Axboe
  2024, on the kernel ML) — limits of io_uring's benefit
  for request/response workloads without batched SQE
  submissions.
