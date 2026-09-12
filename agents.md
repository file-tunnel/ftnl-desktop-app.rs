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
- Run `nix develop --command agent-check` before publishing; it covers headless
  and all-feature format, locked Clippy/tests, dependency validation, actionlint,
  and the repository's Nix checks.
- Integrate shared branch history with merge commits when necessary.

avoid git rebase in favor of git merge.

## Repository-local Git worktrees

- Create or use a Git worktree only when the human operator explicitly authorizes it for the current task. Concurrency or a dirty checkout is not permission by itself.
- Put every authorized worktree at `<repository-root>/tmp/worktrees/<name>`; from the repository root, use `./tmp/worktrees/<name>`. Never place worktrees beside repositories or organization directories.
- Keep `tmp`, `temp`, `tmp/worktrees`, and `temp/worktrees` ignored in the repository-root `.gitignore`. Do not commit files from those directories.
- Relocate or remove a worktree only when the operator explicitly requests it. Before removal, preserve and publish intended changes, verify its commit is represented on the target branch, and confirm there are no tracked, untracked, ignored-sensitive, or in-use files that must survive. Remove it with `git worktree remove <path>` without `--force`; never delete a worktree directory with `rm`.
