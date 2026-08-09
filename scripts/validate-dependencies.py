#!/usr/bin/env python3
"""Keep Zed dependency edges equal to real Cargo dependencies."""

from pathlib import Path
import sys

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    sys.exit(f"Python 3.11+ is required (running {sys.version.split()[0]})")

ROOT = Path(__file__).resolve().parents[1]
EXPECTED = {
    "file-tunnel/ftnl-interfaces": "ftnl-interfaces",
    "file-tunnel/ftnl-clients": "ftnl-client",
    "file-tunnel/ftnl-ui-components": "ftnl-ui-components",
    "oresoftware/next-loggers": "next-loggers",
}


def main() -> int:
    zed = tomllib.loads((ROOT / ".zpkg.toml").read_text(encoding="utf-8"))
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    declared = set(zed.get("dependencies", {}))
    errors: list[str] = []
    if declared != set(EXPECTED):
        errors.append(f"Zed edges differ: expected {sorted(EXPECTED)}, got {sorted(declared)}")
    cargo_dependencies = cargo.get("dependencies", {})
    for edge, package in EXPECTED.items():
        if package not in cargo_dependencies:
            errors.append(f"{edge} has no Cargo dependency named {package}")
    if zed.get("install", {}).get("adapter") != "none":
        errors.append("desktop executable install.adapter must be none")
    if errors:
        print("dependency validation failed:", file=sys.stderr)
        for error in errors:
            print(f" - {error}", file=sys.stderr)
        return 1
    print("validated desktop Cargo/Zed dependency parity")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
