# Arbitrum Reth — Nitro → Rust Port

## What This Project Is
Port Arbitrum Nitro's entire logic to Rust, building on reth as the execution client. The existing reth fork at
~/Documents/RustRoverProjects/reth/crates/arbitrum/ has partial implementations. This repo (arbreth) is the clean target.
It should use the official reth as an SDK / library and not make any changes to reth itself (except zombie accounts if needed).

## MCP Tools — Your Primary Interface
You have 7 MCP tools via `nitro-translator`. Use them constantly:

- `nitro_status` — Check progress. Run at session start and periodically. Use detail="summary" or "by_module".
- `nitro_next` — Get the next entity to implement. Returns Go source, dependencies, existing Rust references, suggested target file. Always pass agent_id="arb-reth-agent".
- `nitro_context` — Deep dive on a specific entity. Use when nitro_next doesn't give enough context.
- `nitro_done` — Mark an entity as implemented after you write the code. Pass the go_entity, rust_file, rust_entity, and agent_id="arb-reth-agent".
- `nitro_claim` — Claim a batch of entities before starting a module. Prevents conflicts.
- `nitro_search` — Semantic search. Use scope="source" for Go code, scope="target" for existing Rust, scope="sdk" for stock reth, scope="both" for all.
- `nitro_log` — Record architectural decisions that should persist across sessions.

## Workflow Per Entity
1. Call `nitro_next` (or `nitro_context` for a specific entity)
2. Read the Go source carefully — understand what it does
3. Check the `existing_rust_refs` — there may already be a partial Rust implementation in the reth fork
4. If `fork_context` is present, it means this entity is from Nitro's geth fork. The fork_context tells you what Nitro changed vs stock geth. Implement the Nitro-specific
   behavior on top of reth's equivalent.
5. Write the Rust implementation
6. Run `cargo check` to verify it compiles
7. Call `nitro_done` with the entity name and target file

## Workflow Per Module
1. Call `nitro_status` with detail="by_module" to pick a module
2. Call `nitro_claim` with strategy="module" and the module_filter to claim all entities
3. Start with structs/types, then methods, then functions
4. Call `nitro_log` to record key architectural decisions (type mappings, naming conventions)
5. Run `cargo check` after each file

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
- You should use md files heavily to also document your progress and making clear when something is outdated (keep it always up to date to help you along the way)

## Important git practices
- Commit with short commit messages and best practices very frequently (for every unit of logic)
- Do not co-author or author commits with claude but use the git bash commands for git interactions
- Do not push to remotes, just commit locally
- Set up a good .gitignore file and don't commit files that are not related to the logic itself (md files and documentations, test scripts for your owm use etc.)

## Key Reference Repos (Indexed in Chronicle)
- **reth fork** (~/Documents/RustRoverProjects/reth) — has partial arbitrum crate with 68 Rust files. STUDY THIS FIRST for patterns.
- **reth-official** (~/Documents/RustRoverProjects/reth-official) — stock reth SDK, for understanding base traits
- **nitro-rs** (~/Documents/RustRoverProjects/nitro-rs) — reference Rust implementation of some Nitro components
- **arb-alloy** (~/Documents/RustRoverProjects/arb-alloy) — Arbitrum-specific alloy types
- **nitro** (~/Documents/GoLandProjects/nitro) - Official Arbitrum nitro implementation which should always be treated as source of truth

## DO NOT
- Skip calling nitro_done after implementing an entity
- Implement entities without checking nitro_next/nitro_context first
- Ignore the existing Rust references — they show proven patterns
- Write code without running cargo check
- Modify files outside this repo
- Run the node against any network. You should work purely based on logic parity until you have full confidence that it will work without issues
- Use reth-fork, arb-alloy, or nitro-rs as a library for this repo