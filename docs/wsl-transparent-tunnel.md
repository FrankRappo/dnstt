# Transparent WSL tunnelling over DNS (redsocks + unbound + sp-client)

How to route **all** TCP traffic of a WSL distro through a DNS tunnel without touching
applications. Addresses are RFC 5737 placeholders.

## Architecture

```text
WSL applications (no per-app configuration)
  │ TCP (including DNS-over-TCP)
  │   └─ iptables nat OUTPUT -> REDIRECT :12345
  │        └─ redsocks  (socks5 to 127.0.0.1:7000)
  │             └─ sp-client (SOCKS5, dedicated user)
  │                  └─ UDP:53 to public resolvers -> tunnel server
  │ DNS: /etc/resolv.conf -> 127.0.0.1:53
  │   └─ unbound (tcp-upstream) -> TCP:53 to 1.1.1.1 / 8.8.8.8
  │        -> caught by the same REDIRECT -> through the tunnel
  │ UDP:443 (QUIC/HTTP3) -> REJECT -> applications fall back to TCP -> tunnel
  │ other UDP -> REJECT (except loopback/LAN and the tunnel user)
```

The tunnel user is excluded by **owner**, not by port: this is what prevents the
TCP:53 feedback loop described below.

## Control

```bash
bash wsl-tunnel-iptables.sh on      # enable everything
bash wsl-tunnel-iptables.sh off     # direct egress
bash wsl-tunnel-iptables.sh status
```

`off` removes the TCP/QUIC rules and stops redsocks; unbound stays (DNS over TCP, direct).
iptables state does not survive a WSL restart — it comes back as OFF, so `sp-client` must be
started again and `on` re-applied.

## Starting the client

Run as a dedicated unprivileged user with a raised fd limit:

```bash
sudo -u spc sh -c 'cd /work/vpn/dnstt/wsl-run && ulimit -n 65536 && \
  nohup ./sp-client --domain tunnel.example.com --listen 127.0.0.1:7000 \
  --config resolvers.toml --cert cert.pem --max-connections 128 >> client.log 2>&1 &'
```

## Verifying that traffic really goes through the tunnel

```bash
curl -s https://api.ipify.org      # must return the tunnel server IP, no proxy env vars
dig +short example.com @127.0.0.1  # DNS via unbound -> tunnel
tail client.log | grep -a PUMP     # client-side protocol counters
```

## Files

| File | Purpose |
|---|---|
| `wsl-tunnel-iptables.sh` | transparent-mode switch (on/off/status) |
| `redsocks.conf` | TCP -> SOCKS5 127.0.0.1:7000 bridge |
| `unbound-slipstream.conf` | resolver config (tcp-upstream) |
| `resolv.conf.direct-backup` | original resolv.conf before switching |
| `pool/pool1000.txt` | pool of working public resolvers, sorted by RTT |
| `pool/make-config.sh N` | generate `resolvers.toml` from the top N |
| `pool/refresh-probe.sh` | re-validate the pool (list -> probe -> top-1000) |
| `pool/probe-sequential.sh` | sequential probing of known resolvers |

## Pitfalls already solved (do not repeat)

1. **TCP:53 feedback loop.** `sp-client` has a DNS-over-TCP fallback. If REDIRECT captures its
   own TCP:53, redsocks feeds it back into the client: connection storm -> "Too many open files"
   -> client dies. Fix: exclude by owner (the tunnel user), never by port.
2. **REJECT on loopback.** The UDP chain must exclude `lo` / `127.0.0.0/8` (and the LAN),
   otherwise `dig` / unbound get "connection refused" on `127.0.0.1:53`.
3. **unbound without a root anchor.** The package did not create a DNSSEC anchor on that host;
   the validator module is disabled and upstreams do the validation.
4. **Immutable resolv.conf.** It had `chattr +i` from other tooling — remove before editing.
5. **Parallel xargs probing of resolvers lies** (drops results). Probe sequentially.

## Measured performance

- Bulk throughput through the tunnel: ~85–120 KB/s (10 MB file in ~87 s), single session.
- Peaks: ~150 KB/s with a mixed 8-resolver config; per-resolver RTT 60–100 ms.
- Sustained ceiling: the server (2 vCPU, ~300 RPS) and resolver rate limits, not the client.
  8 resolvers scale; 12 degrade.
- "Wild" (public open) resolvers: more than one in the config makes it worse — they do not
  sustain the pump RPS and drag the single session down.
- Resolver fatigue: after hours of ~120 RPS duplicate pumping, timeouts grow and throughput
  drops; restarting the client reduces duplicate pressure.
- Heavy TLS (git clone over HTTPS with GnuTLS) can break; retry, use HTTP/1.1, or fetch tarballs
  with curl.
- SSH over the tunnel works; for frequent admin jumps add a RETURN rule for `:22`.

## Resolver rotation (when performance degrades)

```bash
bash pool/probe-sequential.sh              # measure RTT from the current exit
bash pool/make-config.sh 8 > /work/vpn/dnstt/tmp/r.toml
sudo cp /work/vpn/dnstt/tmp/r.toml wsl-run/resolvers.toml
sudo pkill -x sp-client; sleep 2
# start sp-client again (see above), then wsl-tunnel-iptables.sh on
```

With transparent mode enabled, `apt`/`curl` also go through the tunnel (slow). For large
installs: `off` -> install -> `on`.

## Agent/TUI workloads are not viable

Interactive TUI streaming (e.g. an LLM coding agent) over ~100–150 KB/s DNS tunnel hangs on
long API responses. Light HTTPS/curl works; agent TUI does not. Keep the tunnel for
structured, short-lived transfers.
