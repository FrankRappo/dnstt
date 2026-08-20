# Task: DNS Scanner — Phase 1 (Foundation + Probe Protocol)

## Project
`/work/vpn/slipstream-pure-v1.8-windowed-pacing/`

Rust workspace. Crates: sp-common, sp-transport, sp-multipath, sp-client, sp-server.
Domain: `tunnel.example.com`, server: 203.0.113.130, client VPS for testing: 203.0.113.102.

## Read FIRST
- `crates/sp-scanner/PLAN.md` — full plan, read it entirely before coding
- `crates/sp-transport/src/lib.rs` — DNS query/response builders, reuse these
- `crates/sp-transport/src/dns_udp.rs` — UDP transport, reuse pattern
- `crates/sp-transport/src/doh.rs` — DoH transport, reuse for stealth scanning
- `crates/sp-common/src/config.rs` — resolver config format (ResolverConfig, Config)
- `crates/sp-common/src/lib.rs` — constants (DEFAULT_DOMAIN, EDNS0_PAYLOAD_SIZE)
- `crates/sp-server/src/main.rs` — server code, need to add probe handler
- `config/resolvers.toml` — current resolver config format (output format for scanner)

## What to build

### 1. Probe handler in sp-server

Add to server's DNS query handler. When query subdomain starts with "probe-",
respond with TXT containing the source IP of the query.

```
Query:   probe-abc123.tunnel.example.com TXT
Response: TXT "resolver=<source_ip>;time=<unix_ts>;probe=abc123"
```

This lets the scanner discover the REAL recursive resolver behind proxies/forwarders.
Also support probe queries for different record types (A, AAAA, HTTPS, DNSKEY, NULL) —
respond with the appropriate record type containing resolver info.

Small change — find where server handles incoming queries and add before tunnel logic:
```rust
if subdomain.starts_with("probe-") {
    // return probe response with source IP
}
```

### 2. Create crate sp-scanner

Add `"crates/sp-scanner"` to workspace members in root `Cargo.toml`.

`crates/sp-scanner/Cargo.toml`:
```toml
[package]
name = "sp-scanner"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "dns-scanner"
path = "src/main.rs"

[dependencies]
sp-common = { path = "../sp-common" }
sp-transport = { path = "../sp-transport" }
tokio = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
hickory-proto = { workspace = true }
clap = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
rand = { workspace = true }
bytes = { workspace = true }
reqwest = { workspace = true }
```

### 3. CLI structure (clap)

```
dns-scanner <COMMAND>

Commands:
  scan       Discover DNS resolvers (stealth by default, --mode aggressive for full speed)
  validate   Test delegation and record types for found resolvers
  test       Quick reachability test for resolver list
  bootstrap  Auto-discover working DNS path (for restricted networks)
  hints      Help user find hints for bootstrap
  detect     Analyze network environment
  profile    Deep resolver analysis (rate limit, EDNS0, etc.)
  monitor    Continuous monitoring of resolvers

Global options:
  --domain <DOMAIN>    Tunnel domain [default: tunnel.example.com]
  --output <FILE>      Output file
  --verbose            Verbose logging
```

### 4. IP exclusion list module (`src/exclude.rs`)

Function: `is_excluded(ip: Ipv4Addr) -> bool`

Exclude these ranges:
- 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8
- 169.254.0.0/16, 172.16.0.0/12, 192.0.0.0/24, 192.0.2.0/24
- 192.168.0.0/16, 198.18.0.0/15, 198.51.100.0/24, 203.0.113.0/24
- 224.0.0.0/4, 240.0.0.0/4, 255.255.255.255/32

### 5. Bundled resolver list (`src/resolvers.rs`)

Hardcoded list of ~50 known public DNS resolvers with their DoH URLs.
Include at minimum:
- Google (8.8.8.8, 8.8.4.4, doh: dns.google)
- Cloudflare (1.1.1.1, 1.0.0.1, doh: cloudflare-dns.com)
- Quad9 (9.9.9.9, doh: dns.quad9.net)
- OpenDNS (208.67.222.222, doh: doh.opendns.com)
- AdGuard (94.140.14.14, doh: dns.adguard-dns.com)
- NextDNS (45.90.28.0, doh: dns.nextdns.io)
- CleanBrowsing (185.228.168.9, doh: doh.cleanbrowsing.org)
- Mullvad (194.242.2.2, doh: dns.mullvad.net)
- DNS.SB (185.222.222.222, doh: doh.dns.sb)
- Comodo (8.26.56.26)
- Neustar (64.6.64.6)
- Level3 (4.2.2.1, 4.2.2.2)
- Yandex (77.88.8.8)
- And more — include as many as reasonable

Format: `Vec<BundledResolver>` where BundledResolver has ip, name, optional doh_url.

### 6. DNS prober module (`src/prober.rs`)

Two probing methods:

**DoH probe** (stealth, preferred):
- Use reqwest to send POST to DoH endpoint
- Body: DNS wire format query (reuse `sp_transport::build_dns_txt_query`)
- Content-Type: application/dns-message
- Parse response as DNS wire format

**UDP probe** (fallback):
- Reuse `DnsUdpTransport` pattern from sp-transport
- Send DNS A query to ip:53
- Timeout: 3 seconds

**Probe function**:
```rust
async fn probe_resolver(ip: &str, doh_url: Option<&str>, domain: &str) -> ProbeResult {
    // 1. If doh_url available → try DoH first
    // 2. If DoH fails or no URL → try UDP
    // 3. Return: alive, delegation_works, latency, method_used
}
```

**Delegation test**:
```rust
async fn test_delegation(ip: &str, doh_url: Option<&str>, domain: &str) -> DelegationResult {
    // Send TXT query for probe-<random>.tunnel.example.com
    // Parse response: extract "resolver=..." from TXT
    // Return: works (bool), real_resolver_ip, latency
}
```

### 7. Scanner logic (`src/scanner.rs`)

**Stealth mode** (default):
```
1. Load bundled resolver list
2. For each resolver (randomized order):
   a. If has DoH URL → probe via DoH (invisible)
   b. Else → probe via UDP (rate limited: 1-2 rps)
3. Concurrency: 10 DoH workers, 2 UDP workers
4. Progress: [123/500] doh=89 udp=34 alive=76 delegation=52
5. Output: alive.txt (one IP per line)
```

**Aggressive mode** (`--mode aggressive`):
```
1. Load bundled list + optionally download public-dns.info/nameservers.txt
2. Probe all via UDP, no DoH preference
3. Concurrency: 50-100 workers
4. Rate: 1000+ rps
5. Progress: [4521/7000] alive=312 delegation=189
```

### 8. Validate command (`src/validate.rs`)

Input: alive.txt from scan.
For each alive resolver:
1. Delegation test (probe-XXX.tunnel.example.com → check TXT response)
2. Record type test: send query for each type (TXT, HTTPS, DNSKEY, NULL, A, AAAA)
   - For each type: does the response arrive? Is it intact? What's the max size?
3. EDNS0 size test: queries with buffer size 512, 1232, 4096
4. Latency: 3 probes, average

Output: resolvers-validated.toml in project format:
```toml
[[resolver]]
name = "8.8.8.8"
ip = ["8.8.8.8"]
doh = "https://dns.google/dns-query"
latency_ms = 45
edns0_max = 4096
supported_types = ["TXT", "HTTPS", "DNSKEY", "A", "AAAA"]
best_type = "HTTPS"
```

### 9. Test command (`src/test_cmd.rs`)

Quick reachability test. Input: resolver list (toml).
Test each resolver: is it reachable RIGHT NOW?
Output: resolvers-local.toml (only reachable ones, sorted by latency).

### 10. Build and verify

After writing all code:
1. `cargo build` — must compile with zero errors
2. `cargo build --release` — release build
3. Fix any warnings
4. Verify CLI: `./target/debug/dns-scanner --help` shows all commands
5. Verify subcommands: `./target/debug/dns-scanner scan --help` etc.

## Constraints

- Reuse sp-transport and sp-common where possible, don't duplicate DNS code
- Pure Rust, no C dependencies
- All async with tokio
- Stealth mode is DEFAULT, aggressive is opt-in
- DoH probing preferred over UDP (invisible to ISP)
- Use tracing for logging (like the rest of the project)
- Don't modify existing crates unless necessary (only sp-server for probe handler)
- Commit after each major piece works

## File structure

```
crates/sp-scanner/
  Cargo.toml
  PLAN.md              ← already exists, don't modify
  src/
    main.rs            ← CLI entry point (clap)
    exclude.rs         ← IP exclusion list
    resolvers.rs       ← bundled resolver list
    prober.rs          ← DoH + UDP probing
    scanner.rs         ← scan command logic (stealth + aggressive)
    validate.rs        ← validate command
    test_cmd.rs        ← test command
    bootstrap.rs       ← placeholder (Phase 2)
    detect.rs          ← placeholder (Phase 2)
    profile.rs         ← placeholder (Phase 2)
    monitor.rs         ← placeholder (Phase 2)
```
