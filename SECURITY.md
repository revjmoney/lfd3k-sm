# Security Policy

## Reporting a vulnerability

Please report security problems privately here:
https://github.com/revjmoney/lfd3k-sm/security/advisories/new

Do not open a public issue for anything sensitive. Include what you found and the steps to
reproduce it.

## What is in place

- Private vulnerability reporting (the link above)
- Dependabot alerts and Dependabot security updates
- Dependabot version updates, weekly, for GitHub Actions and Cargo crates
- Secret scanning with push protection
- CodeQL code scanning (default setup)
- CI on every push and pull request: the Rust workspace build, tests and clippy plus the Python tool tests (host side; the ESP32-S3 firmware is built by the release workflow on tags), plus a guard job that fails if private data (home network addresses, personal email, tokens or keys) lands in the tree

[Rev. J. Money - 2026] · by JMSCNC, makers of RotatoCAM
