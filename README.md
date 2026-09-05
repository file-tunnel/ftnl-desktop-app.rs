# ftnl-desktop-app.rs

Native Rust desktop workspace for File Tunnel. It provides a regular egui
window plus a system-tray lifecycle, an opt-in in-memory clipboard history, and
the existing short-lived peer-to-peer file receiver. The Rust and Flutter
desktop apps are deliberately separate implementations of the same product
contract; neither is a reduced or secondary edition.

The current clipboard slice supports bounded plain-text capture, pause/resume,
case-insensitive search, pins, deletion, clear-unpinned, content
deduplication, and age/count retention. Capture starts paused, closing the
window hides it only when a working tray is available, and quitting is always
explicit from the tray. Images, rich text, clipboard-file capture, encrypted
persistence, global shortcuts, and platform source-application discovery are
not implemented yet and must not be inferred from this milestone.

## Architecture and repository boundaries

| Dependency | Responsibility used here |
|---|---|
| [`ftnl-interfaces`](https://github.com/file-tunnel/ftnl-interfaces) | canonical transfer vocabulary and paired desktop workspace contract |
| [`ftnl-clients`](https://github.com/file-tunnel/ftnl-clients) | HTTP routes, authorization, TLS/timeout/redirect policy, and redacted errors |
| [`ftnl-ui-components`](https://github.com/file-tunnel/ftnl-ui-components) | host-owned picker lifecycle and optional egui renderer |
| [`ores-otel/ores.otel.log`](https://github.com/ores-otel/ores.otel.log) | structured `next-loggers/v1` records through its OpenTelemetry adapter |

The library half is headless and testable. `src/workspace.rs` is the pure,
optimistic-revision clipboard reducer. `contracts/desktop-feature-manifest.json`
is checked against its closed feature vocabulary in unit tests and matches the
Flutter manifest through the schema and conformance tests in
`ftnl-interfaces`. `src/transfer.rs` owns process-only secret wrappers and safe
output persistence. The optional `native-ui` feature adds one egui event loop,
one native tray, and one single-threaded Tokio network worker; the UI thread
never blocks on HTTP.

The app intentionally does not import `ftnl-lib-core`: schema/DDL/ORM generation
belongs in servers and build tooling, not a transfer client. It also does not
persist `ftnl-sync` jobs yet, because protocol v1 has no multipart download
resume contract. Snapshot reconciliation is safe; claiming byte-offset resume
would not be.

## Security model

- Pairing URIs and desktop capabilities live only in zeroizing process memory.
- QR contents are rendered directly; there is no automatic pairing-material
  clipboard write, screenshot, analytics, or persistence path.
- Clipboard capture is explicit, paused by default, text-only, size bounded,
  content-hash checked, and process-local. UI and error paths never log or
  interpolate captured content.
- Source-fingerprint exclusions fail closed when a platform adapter supplies a
  fingerprint. The current generic adapter cannot discover the source
  application, and the UI states that limitation.
- `secure_bluetooth` provides the app-layer proximity substrate: ephemeral
  X25519, transcript-bound HKDF-SHA256, explicit SAS confirmation, and
  directional ChaCha20-Poly1305 frames for opaque Shared Auth step-up, peer
  information, and signed update-manifest payloads. Bluetooth remains an
  untrusted bearer and is never treated as proof of identity, MFA strength, or
  product authorization. Shared Auth remains the identity boundary. Native
  adapters and permissions stay disabled until the acceptance gate is met.
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

## Desktop lifecycle

The regular application window exposes three first-class pages: Clipboard,
Receive files, and Privacy & retention. Tray actions can open or hide the
window, pause or resume capture, or explicitly quit. A window-manager close is
cancelled and converted into a hide only after tray initialization succeeds;
otherwise close exits normally so the process cannot become unreachable.

Linux native builds require GTK 3 and Ayatana AppIndicator development
headers. The Nix shell and GitHub Actions install those dependencies.

## Formally verified lifecycle

Every user command and worker response crosses the pure reducer in
`src/lifecycle.rs`. The reducer owns the control state and the single active
operation identifier, rejects illegal commands, and treats stale or duplicate
responses as no-ops. UI stage, busy state, retry behavior, and session
authority are derived from that one state instead of being mutated
independently.

`formal/DesktopLifecycle.tla` exhaustively checks the bounded control
abstraction, including operation-ID wraparound. The product tests exercise the
same transition API from `ftnl-ui-components` and verify response correlation.
Formal artifacts contain no pairing URI, capability, filename, file ID, path,
bytes, or raw transport error. See `formal/README.md` for the exact proof
boundary.

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
