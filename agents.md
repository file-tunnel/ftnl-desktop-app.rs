# File Tunnel Rust desktop agent instructions

These instructions apply to this repository and every directory beneath it.

- Pairing URIs, capabilities, event tickets, filenames, file IDs, local paths,
  and bytes are sensitive. Never log, serialize, persist, or include them in
  error messages, analytics, screenshots, or clipboard automation.
- Keep network work off the egui event loop. All File Tunnel HTTP behavior must
  go through `ftnl-client`; do not duplicate routes, authorization headers,
  redirect policy, or response parsing.
- Use `ftnl-interfaces` for wire vocabulary and `ftnl-ui-components` for picker
  state/rendering. Every Zed edge must also be a real Cargo dependency.
- Use the ORES logger through its OpenTelemetry adapter. Log constant event
  names and bounded counts only.
- Preserve declared-size checks, safe default filenames, same-directory staging,
  flush-before-persist, atomic output, and no-clobber-by-default behavior.
- Do not claim byte-range or crash resume until the shared protocol defines it.
- Run headless and all-feature format, locked Clippy/tests, dependency validation,
  actionlint, and the Nix agent check before publishing.
