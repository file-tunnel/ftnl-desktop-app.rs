# Security policy

Report vulnerabilities through GitHub Security Advisories. Do not open a
public issue containing a pairing URI, capability, event ticket, tunnel/file
identifier, filename, local path, log record from a private deployment, or file
contents.

The desktop app must never persist or log pairing material. A future feature
that survives process restart must use the operating-system credential vault
and a protocol contract that defines safe resume semantics; plaintext files,
environment variables, generic app preferences, and the sync metadata database
are not credential stores.
