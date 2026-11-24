# Server runtime layout and operations

Reference description of a working single-server Slipstream Pure v3.0 deployment.
All addresses are RFC 5737 placeholders; the real deployment uses a dedicated VPS.

## Layout

```text
/opt/slipstream/
├── sp-server        # static Linux binary, v3.0 build
├── cert.pem         # self-signed server certificate (CN=slipstream)
├── key.pem          # private key, mode 600
└── tmp/             # diagnostics (tcpdump captures), not required for runtime
```

## systemd unit

`/etc/systemd/system/slipstream-server.service`:

```ini
[Unit]
Description=Slipstream Pure DNS Tunnel Server v3.0
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/slipstream
ExecStart=/opt/slipstream/sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 --cert /opt/slipstream/cert.pem --key /opt/slipstream/key.pem
Restart=always
RestartSec=2
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

Notes:

- `--dns-listen` binds only the public address, so `systemd-resolved` on `127.0.0.53:53` keeps working.
- `Restart=always` + `RestartSec=2` makes the service survive crashes without supervision.
- v3 is the egress proxy itself; there is no `--target-address` and `reset.seed` is unused.

## Ports

| Port | Process | Purpose |
|---|---|---|
| `<public-ip>:53/udp` + `/tcp` | `sp-server` | tunnel |
| `127.0.0.53:53` | systemd-resolved | local resolver, untouched |
| `:22/tcp` | sshd | administration |

A legacy v2 setup additionally used a local SOCKS5 daemon (Dante on `127.0.0.1:1080`);
v3 does not need it.

## Verification

```bash
systemctl is-active slipstream-server
ss -ulnp | grep sp-server
ss -tlnp | grep sp-server

# Delegation is served by Cloudflare; SOA/NS to the tunnel server itself
# answer SERVFAIL/Refused by design — it only answers tunnel qnames.
dig +short NS tunnel.example.com @8.8.8.8

# End-to-end through a running client:
curl -x socks5h://127.0.0.1:7000 -s https://api.ipify.org   # -> server public IP
```

## Certificate

Self-signed `CN=slipstream`, validity 12 months. The client pins/validates with the same
`cert.pem`, so rotation means: generate a new pair, install on the server, restart the
service, distribute the new `cert.pem` to every client, restart clients.

Track the expiry date; an expired certificate breaks all clients at once.

## Logs

- Primary: `journalctl -u slipstream-server` (DEBUG; logs `DNS error … no query` for junk queries).
- The v3 server writes little per connection; protocol-level counters are on the client (`PUMP` lines).

## Backup policy

Keep a local copy of everything needed to rebuild the server without SSH:

- `sp-server` binary + its SHA-256;
- `cert.pem`, `key.pem` (private — never in a public repository);
- the systemd unit file;
- optional: recent logs.

Restore order: files to `/opt/slipstream/` (key mode 600) → unit to
`/etc/systemd/system/` → `systemctl daemon-reload` → `systemctl enable --now slipstream-server`.

## Capacity

A 2 vCPU / 2 GB VPS with this binary saturates around ~300 DNS RPS; the practical client
configuration is 8–12 resolvers. More resolvers degrade throughput instead of scaling it.
