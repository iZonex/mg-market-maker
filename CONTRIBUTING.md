# Contributing

Thank you for your interest in contributing to Market Maker! This document provides guidelines for contributing.

## Getting Started

1. **Fork** the repository
2. **Clone** your fork: `git clone https://github.com/YOUR_USERNAME/market-maker.git`
3. **Create a branch**: `git checkout -b feature/your-feature`
4. **Install Rust**: `rustup default stable`
5. **Build**: `cargo build`
6. **Test**: `cargo test`

## Development Workflow

### Before Submitting

```bash
# All must pass
cargo test --all
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

### Code Style

- **Rust edition 2021** — standard modern Rust
- **`rust_decimal::Decimal`** for all money/price/quantity — never `f64`
- **Meaningful names** — `inventory_manager` not `im`, `mid_price` not `mp`
- **Tests alongside code** — `#[cfg(test)] mod tests` in each module
- **Tracing for logging** — `info!`, `warn!`, `error!` from the `tracing` crate

### Adding a New Strategy

1. Create `crates/strategy/src/your_strategy.rs`
2. Implement the `Strategy` trait:
   ```rust
   impl Strategy for YourStrategy {
       fn name(&self) -> &str { "your-strategy" }
       fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> { ... }
   }
   ```
3. Export from `crates/strategy/src/lib.rs`
4. Add variant to `StrategyType` enum in `crates/common/src/config.rs`
5. Wire into `run_symbol()` in `crates/server/src/main.rs`
6. Add tests and benchmarks
7. Document in README.md

### Adding a New Exchange Connector

1. Create `crates/exchange-yourexchange/`
2. Implement `ExchangeConnector` trait (see `crates/exchange-core/src/connector.rs`)
3. Handle authentication, WebSocket, rate limits
4. Add to workspace in root `Cargo.toml`
5. Add tests (mock server recommended)

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add OKX exchange connector
fix: correct VPIN bucket overflow on low volume
perf: optimize GLFT C2 computation
docs: add strategy comparison table
test: add integration tests for kill switch
```

## Architecture Decisions

### Why Rust?

Market making is latency-sensitive. Rust gives us:
- Zero-cost abstractions
- No GC pauses
- Memory safety without runtime overhead
- `Decimal` arithmetic without floating-point errors

### Why `rust_decimal` instead of `f64`?

Financial arithmetic requires exact decimal representation. `0.1 + 0.2 == 0.3` must be true. With `f64`, it isn't. A single rounding error compounded over millions of operations leads to real money loss.

### Why Avellaneda-Stoikov / GLFT?

These are the mathematically optimal solutions to the market making problem under specific assumptions. They provide a rigorous framework rather than ad-hoc heuristics. The GLFT model adds practical execution risk to the theoretical A-S framework.

## Questions?

Open an issue with the `question` label.
