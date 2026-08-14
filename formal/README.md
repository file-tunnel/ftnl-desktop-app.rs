# Desktop receive lifecycle model

`DesktopLifecycle.tla` is the finite abstraction of the production Rust
reducer in `src/lifecycle.rs`. It checks session ownership, single-operation
ownership, deterministic recovery, terminal cleanup, and stale-response
stuttering across a bounded wraparound operation-ID space.

The operation-ID bound does not cap the application. It makes wraparound and
identifier reuse reachable during exhaustive model checking while production
uses a nonzero 64-bit counter. The production conformance tests additionally
exercise invalid commands, duplicate responses, and mismatched response IDs.

The model contains no pairing URI, capability, filename, file identifier,
path, bytes, or transport error. Those values stay outside formal artifacts and
logs. The proof covers the finite control abstraction and its refinement seam;
it does not prove the network service, OS filesystem, renderer, or dependencies.

Run the proof and implementation checks with:

```sh
nix develop --command agent-check
```
