# Public export policy

This repository is generated from a private operational workspace.

## Included

- all maintained Rust source trees under `slipstream-pure/`;
- the current `v3.0-single-session` protocol source and research notes;
- legacy source revisions and reproducible documentation;
- public scripts after endpoint and credential sanitization;
- upstream `Fox-Fig/slipstream-rust-plus` as a Git submodule.

## Deliberately excluded

- production passwords, API tokens and server inventory files;
- TLS private keys, runtime certificates and reset seeds;
- private server-maintenance archives and their manifests;
- nested `.git` directories and private commit history;
- Rust `target/`, C build trees, old multi-gigabyte tar archives;
- compiled binaries, which may embed operational endpoints or build paths.

## Placeholder map

Production endpoints are replaced by RFC 5737 examples such as
`203.0.113.130`, `203.0.113.181`, and `203.0.113.102`. The production DNS zone is
replaced by `example.com` / `tunnel.example.com`. Placeholder configurations are
not deployable until unique credentials and domains are generated.

## Upstream test fixtures

The `slipstream-rust-plus` submodule is an independently maintained public
upstream repository and may contain its own documented test certificates. Those
fixtures are not production credentials from this workspace.
