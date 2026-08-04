# Katherine Engine

A Rust-native, local-first memory engine for AI agents. Math-grounded: power-law decay, decision-boundary retrieval, immutable event log.

## Structure

- `katherine-core` — zero-dependency types and traits
- `katherine-engine` — provider, tools, agent loop, memory, persistence
- `katherine-cli` — entry point, wake.rs system prompt assembly

## Quick Start

```bash
cargo build --release
./target/release/katherine-cli "Hello"
```

## Design

See [SPEC.md](SPEC.md) for the full architecture specification.

## License

MIT

