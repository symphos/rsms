# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 语言要求 (Language Requirement)

`AGENTS.md` 规定：**全程思考过程和最终输出的所有回答内容必须使用中文**（仅代码语法本身的英文关键词除外）。在本仓库工作时遵守此约定。

## What this is

RSMS is a Rust multi-protocol SMS gateway middleware framework supporting four Chinese carrier SMS protocols: **CMPP 2.0/3.0** (China Mobile), **SMGP 3.0.3** (China Telecom), **SMPP 3.4/5.0** (SMPP.org), and **SGIP 1.2** (China Unicom). It is a Cargo workspace (edition 2024, Rust 1.85+), version `0.0.1`, not yet published. Both server (`serve()`) and client (`connect()`) roles are supported, with per-account connection pooling, rate limiting, sliding-window request/response matching, and long-message (multi-part SMS) split/merge.

## Common commands

```bash
# Build / lint
cargo build --workspace
cargo clippy --workspace        # must be warning-free (CONTRIBUTING requirement)

# Unit tests (library tests live in the framework crates)
cargo test --workspace --lib
cargo test -p rsms-core -p rsms-connector -p rsms-longmsg

# Integration + stress tests live in the `rsms-tests` package (crate name = rsms-tests).
# Run an individual test target by its [[test]] name from tests/Cargo.toml:
cargo test -p rsms-tests --test cmpp-integration
cargo test -p rsms-tests --test smgp-integration
cargo test -p rsms-tests --test smpp-integration
cargo test -p rsms-tests --test sgip-integration

# Stress / long-message / dynamic-connection tests need --nocapture to see throughput output:
cargo test -p rsms-tests --test cmpp-stress-test -- --nocapture
cargo test -p rsms-tests --test cmpp-multi-account-stress-test -- --nocapture
cargo test -p rsms-tests --test cmpp-longmsg-test -- --nocapture
cargo test -p rsms-tests --test cmpp-dynamic-connection-test -- --nocapture
```

> Note: tests live in the single `rsms-tests` package (`tests/Cargo.toml`), not the old per-protocol `cmpp-endpoint-example` packages. Use the `-p rsms-tests --test <name>` form above; test target names follow `<proto>-<kind>` (e.g. `smpp-multi-account-stress-test`). README.md / CONTRIBUTING.md / docs/reference/01-tests.md already use this form.

**Stress testing rule (critical):** stress tests MUST run with log level `WARN`. At `INFO`, a 300s run emits 2.4M+ log lines and throughput collapses from ~12,500 TPS to ~2,700. `EndpointConfig` in the stress tests is already configured with `.with_log_level(WARN)` — do not lower it.

## Architecture (the big picture)

The data flow through a single connection is: **frame decode → protocol message decode → session bookkeeping → business handler chain** (see `run_connection` in `crates/rsms-connector/src/connection.rs`).

### Crate layout (workspace members under `crates/`)

| Crate | Role |
|-------|------|
| `rsms-core` | Foundational types: `Frame`, `RawPdu`, `EncodedPdu` trait, `EndpointConfig`, `IdGenerator` trait, `CString`/`PString` helpers |
| `rsms-connector` | The orchestration crate. Server `ServerBuilder`, client `ClientBuilder`, `AccountPool`, `MessageSource`, per-protocol handlers, `TransactionManager` |
| `rsms-business` | `BusinessHandler` trait + `InboundContext` (where business logic processes inbound PDUs) |
| `rsms-codec-{cmpp,smgp,smpp,sgip}` | Per-protocol encode/decode: header parsing + PDU (de)serialization |
| `rsms-longmsg` | Long-message `LongMessageSplitter` / `LongMessageMerger` (8-bit and 16-bit UDH) |
| `rsms-window` | Sliding window for request/response matching (`offer` vs `try_offer`) |
| `rsms-ratelimit` | Token-bucket rate limiter |
| `rsms-session` | Connection state machine + heartbeat |
| `rsms-pipeline` | Processing pipeline primitives |

`tests/` is the `rsms-tests` package (integration + stress, with `tests/common/` = `rsms-test-common` shared helpers). `examples/` holds runnable per-protocol `*_server` / `*_client` binaries.

### Key design decisions (these shape how the framework behaves)

- **The framework does NOT auto-send SubmitResp/SubmitSmResp.** The per-protocol handlers return `Continue` on inbound Submit; the user's `BusinessHandler::on_inbound` is responsible for writing the response via `ctx.conn.write_frame()`. The framework also does not cache MsgIds (avoids OOM). Server pattern: receive Submit → immediately return Resp → asynchronously enqueue.
- **Outbound messages come from a user-owned queue via the `MessageSource` trait.** `fetch(account, batch_size) -> Vec<MessageItem>`; the user owns serialization. The framework's `run_outbound_fetcher` batch-fetches (16 at a time) and `write_frame`s them (it does NOT go through the window). **The MessageSource push/fetch key must equal the endpoint ID** — `run_outbound_fetcher` keys by `conn.authenticated_account()`, which returns `endpoint.id`. `MessageItem::Single(Vec<u8>)` is a normal SMS; `MessageItem::Group { items }` is a long-message group, and the framework guarantees all frames in a group go out in order on the same connection.
- **ID generation is per-account via the `IdGenerator` trait** (`rsms-core`). `AccountConnections` holds an `Arc<dyn IdGenerator>` (`next_msg_id() -> u64`, `next_sequence_id() -> u32`); default impl is `SimpleIdGenerator` (`rsms-connector`). Business handlers reach it through `InboundContext.id_generator: Option<Arc<dyn IdGenerator>>`. (The old `Protocol` trait and per-handler global static counters were dead code and have been removed.)
- **`TransactionManager`** (`rsms-connector/src/transaction/`, per-protocol submodules) is the client-side helper for matching Submit → Resp → Report. It keys transactions by `sequence_id`, then re-keys to `msg_id` once the resp arrives, and drives a `MessageCallback`. Clients maintain their own msgId → business-info mapping and remove entries once matched.
- **Concurrency:** `DashMap` is used in place of `RwLock<HashMap>` in hot paths to cut lock contention.
- **Client request sending:** `client.rs::send_request` uses `window.offer()` (waits when full) rather than `try_offer()` (errors immediately). Default `window_size` is only 16 — stress/high-throughput clients must set `.with_window_size(2048)`.

### Per-protocol gotchas (header offsets matter)

Header lengths differ, which directly affects where `sequence_id` is parsed:

| | CMPP | SMGP | SMPP | SGIP |
|---|---|---|---|---|
| Header length | 12B | 12B | 16B | 20B |
| Auth | MD5 | MD5 | plaintext | plaintext |
| Status report | via Deliver | via Deliver | via DeliverSm (`esm_class=0x04`) | standalone Report command |
| Heartbeat | ActiveTest | ActiveTest | EnquireLink | none |
| MsgId | 8B binary `[u8;8]` | 10B custom | C-string (`String`) | SgipSequence |

- **`sequence_id` offset:** CMPP/SMGP at bytes 8–11; SMPP/SGIP at bytes 12–15. Both `send_request` (client) and `decode_frames_drain` (server) derive the offset from `endpoint.protocol.seq_offset()` (`Protocol::Smpp`/`Protocol::Sgip` → 12, else → 8). **SMPP clients MUST set `.with_protocol(Protocol::Smpp)`** on `EndpointConfig`, or the protocol defaults to `Protocol::Cmpp` and sequence extraction breaks. (`protocol` is now the enum `rsms_core::Protocol`, not a string, so typos/omissions fail at compile time instead of silently degrading.)
- **Partial PDUs across reads:** the server accumulates into a `Vec<u8>` buffer and consumes via the drain pattern (`decode_frames_drain`); a single read may not contain a whole PDU.
- **Handlers must return `Continue`** for ActiveTest/EnquireLink (returning `Stop` drops the connection).
- SMPP version differences (V3.4 vs V5.0) are only field-length limits, not different PDU structs.

### Switching protocol in user code (3 changes)

`.with_protocol(Protocol::Cmpp | Protocol::Smgp | Protocol::Smpp | Protocol::Sgip)` (with `use rsms_core::Protocol;` or `use rsms_connector::Protocol;`), swap the `Decoder` (`CmppDecoder` / `SmgpDecoder` / `SmppDecoder` / `SgipDecoder`), and import codec types from the matching `rsms-codec-*` crate.

## Graceful shutdown for zero message loss

Stress tests prove zero-loss by stopping in 5 ordered phases (a barrier between each): (1) stop sender tasks, (2) wait for SubmitResp to catch up (resp ≥ sent, 10–15s timeout), (3) drain the MessageSource queue and flush all Reports, (4) stop Report/MO generators, (5) wait for all Reports/MOs to arrive. Count a message as sent only AFTER `send_request` succeeds, never before.

## Project conventions

- Public APIs require doc comments (`///` or `//!`).
- Branch model (CONTRIBUTING.md): work off `develop`, branch `feature/*` | `fix/*` | `explore/*`, PR into `develop`. Conventional-commit prefixes (`feat`/`fix`/`refactor`/`docs`/`test`/`chore`). The current default branch in this checkout is `main`.
- `openspec/` holds spec-driven change proposals (`.cursor/` and `.opencode/` carry the OpenSpec tooling). `docs/` has per-protocol and per-feature guides.

## Reference

`AGENTS.md` is the most detailed living record of the refactor goals, discovered bugs, and per-protocol completion status — read it when working on the connector internals or stress tests. Protocol guides are in `docs/guides/` and `docs/protocols/`; raw protocol specs are in `docs/specs/`.
