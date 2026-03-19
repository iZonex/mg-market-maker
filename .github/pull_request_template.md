## What

<!-- One-line summary of the change -->

## Why

<!-- Motivation: what problem does this solve? Link issue if applicable -->
Closes #

## Changes

-
-

## Risk assessment

- [ ] Affects order placement / cancellation logic
- [ ] Affects risk management (kill switch, inventory limits, etc.)
- [ ] Affects PnL calculation
- [ ] Changes exchange API interaction
- [ ] Config format change (migration needed)
- [ ] None of the above

## Testing

- [ ] Unit tests added / updated
- [ ] Integration tests pass
- [ ] Tested in paper mode
- [ ] Tested on testnet

## Checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -Dwarnings` passes
- [ ] `cargo test --all` passes
- [ ] No new `TODO` or `unwrap()` in hot paths
- [ ] `Decimal` used for all monetary values (no f64)
