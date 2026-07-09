# Security policy

## Reporting a vulnerability

Please report security issues privately to **vasiliy.andronov@gmail.com** rather than
opening a public issue. Include steps to reproduce and, if possible, an assessment of
impact. You'll get an acknowledgement, and we'll coordinate a fix and disclosure.

## Scope

This repository is the **desktop client**. The hosted backend (the AI coach) is a separate
service and is out of scope here — report backend issues to the same address.

## Security model (how to reason about this client)

- The app runs a **localhost-only** GSI listener (`127.0.0.1:53210`) authenticated with a
  per-install token; it is not reachable off the machine.
- Screen reading is **off by default** and, when on, captures **only the Dota window**.
- The app talks only to the Vael backend over an encrypted connection; you can audit the
  exact payloads in `ui/app.js` and `src-tauri/src/`.
- Auto-updates are verified with a minisign signature against the public key in
  `src-tauri/tauri.conf.json`. The private signing key is held only in the release CI's
  secrets and is never in this repository.
