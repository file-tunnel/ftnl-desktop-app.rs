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

<!-- BEGIN ores-agents-pointer: managed by ORESoftware/my-ai; edit there, not here -->

## Canonical agent instructions

Before doing anything else in this repository, also read:

    .ores/agents/AGENTS.md

That path is a symlink to `~/codes/oresoftware/my-ai/AGENTS.md`, whose canonical copy is
<https://github.com/ORESoftware/my-ai/blob/main/AGENTS.md>.

It exists at a fixed path *inside* the repository because some agents cannot walk up past
the repository root, so machine-wide instructions one or more directories above are
invisible to them. This pointer plus that path make the same file reachable from a working
directory anywhere in the tree.

The symlink is deliberately **not committed**: it names an absolute path that is only valid
on a machine with `~/codes/oresoftware/my-ai` checked out, so committing it would produce a
broken link for everyone else and for CI. `.ores/` is git-ignored for that reason. If
`.ores/agents/AGENTS.md` is missing on your machine, create it with:

    mkdir -p .ores/agents
    ln -sfn "$HOME/codes/oresoftware/my-ai/AGENTS.md" .ores/agents/AGENTS.md

or run `~/codes/oresoftware/my-ai/scripts/link-repo-agents.sh` once to do it for every git
repository under `~/codes`, and `--check` to verify them.

A missing `.ores/agents/AGENTS.md` is a setup gap on the reader's machine, never a reason to
skip the canonical instructions: fetch them from the URL above instead.

<!-- END ores-agents-pointer -->
