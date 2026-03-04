# Arbitrum Reth — Nitro → Rust Port

## What This Project Is
Port Arbitrum Nitro's entire logic to Rust, building on reth as the execution client.

## Architecture Decisions (Already Made)
- Target structure: `crates/` workspace with sub-crates mirroring Nitro's module structure
- Use reth's trait system: `StateProvider`, `BlockExecutor`, `EvmConfig`
- Go `BackingStorage` maps to reth's `StateProvider` trait
- Go `storage.StorageBackedXxx` types use reth's state trie storage
- Precompiles implement reth's `Precompile` trait
- Go's `TxProcessor` → reth's `BlockExecutor` implementation
- ArbOS versioning: use Rust enums for version-gated behavior

## Code Standards
- Follow existing reth patterns — look at how reth structures crates
- Use `alloy-primitives` types (Address, B256, U256), not raw [u8; 32]
- Use `thiserror` for error types
- Use `#[derive(Debug, Clone)]` on structs
- Document public APIs with doc comments
- No `unwrap()` in library code — use `?` and proper error types
- Do not make any specific references to nitro in comments and do not have too detailed comments
- Follow reth and nitro logging best practices and cli params

## Important git practices
- Commit with short commit messages and best practices very frequently (for every unit of logic)
- Do not co-author or author commits with claude but use the git bash commands for git interactions

## Key Reference Repos
- **reth fork** (tiljrd/reth) — has partial arbitrum crate with 68 Rust files. STUDY THIS FIRST for patterns.
- **reth-official** — stock reth SDK, for understanding base traits
- **nitro** (tiljrd/nitro) - Official Arbitrum nitro implementation which should always be treated as source of truth

## DO NOT
- Ignore the existing Rust references — they show proven patterns
- Write code without running cargo check
- Modify files outside this repo
- Use tiljrd/reth, arb-alloy, or nitro-rs as a library for this repo
