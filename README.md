# ftnl-desktop-app.rs

Native Rust desktop companion for File Tunnel. It creates a short-lived receive
tunnel, renders the pairing QR code, reconciles server snapshots, and writes a
selected completed file atomically. It complements the Flutter desktop build:
this app is a small native receiver, while Flutter remains the full shared
mobile/desktop application surface.

## Architecture and repository boundaries

| Dependency | Responsibility used here |
|---|---|
| [`ftnl-interfaces`](https://github.com/file-tunnel/ftnl-interfaces) | canonical transfer status vocabulary |
| [`ftnl-clients`](https://github.com/file-tunnel/ftnl-clients) | HTTP routes, authorization, TLS/timeout/redirect policy, and redacted errors |
| [`ftnl-ui-components`](https://github.com/file-tunnel/ftnl-ui-components) | host-owned picker lifecycle and optional egui renderer |
| [`ores-otel/ores.otel.log`](https://github.com/ores-otel/ores.otel.log) | structured `next-loggers/v1` records through its OpenTelemetry adapter |

The library half is headless and testable. `src/transfer.rs` owns process-only
secret wrappers and safe output persistence. The optional `native-ui` feature
adds one egui event loop and one single-threaded Tokio network worker; the UI
thread never blocks on HTTP.

The app intentionally does not import `ftnl-lib-core`: schema/DDL/ORM generation
belongs in servers and build tooling, not a transfer client. It also does not
persist `ftnl-sync` jobs yet, because protocol v1 has no multipart download
resume contract. Snapshot reconciliation is safe; claiming byte-offset resume
would not be.

## Security model

- Pairing URIs and desktop capabilities live only in zeroizing process memory.
- QR contents are rendered directly; there is no automatic clipboard write,
  screenshot, analytics, or persistence path.
- Public cleartext endpoints, redirects, and unbounded HTTP calls are rejected
  by the shared client.
- Server filenames become defaults only when they are exactly one normal path
  component. Explicit destinations are staged beside the destination, flushed,
  declared-size checked, and persisted atomically.
- Existing files are not replaced unless the user explicitly enables atomic
  replacement.
- ORES OTEL records contain constant event names and bounded counters only—no
  tunnel/file IDs, filenames, paths, pairing material, capabilities, or bytes.

Public release artifacts still require platform signing/notarization. Current
GitHub Actions prove source portability and compile the native shell; they do
not publish unsigned binaries as a release.

## Build and test

```bash
cargo test --no-default-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo run --bin ftnl-desktop

nix develop --command agent-check
```

The default endpoint is `https://api.file-tunnel.dev`. It is editable before a
session starts so local development can use loopback or an in-cluster service
name; public `http://` remains fail-closed.

MIT licensed.
