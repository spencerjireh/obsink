# Security policy

ObSink is end-to-end encrypted sync software. A flaw in key derivation, the wire format,
conflict gating, or the server's envelope encryption is a security issue even when no data
is known to be exposed.

## Reporting a vulnerability

Do not open a public issue. Use GitHub's private vulnerability reporting:
<https://github.com/spencerjireh/obsink/security/advisories/new>. Include the component
(core, cli, server, desktop, ios), a reproduction or proof of concept, and the version or
commit. You will get an acknowledgement within 7 days; fixes ship as a normal release and the
advisory is published once a fixed version exists.

## Scope

In scope: everything in this repository, including build and release tooling. Out of scope:
an operator's own hosting (TLS proxy, Postgres exposure, backups) and lost passphrases, which
are unrecoverable by design (AGENTS.md hard rule 5).

## Supported versions

Only the latest release and `main` receive fixes.
