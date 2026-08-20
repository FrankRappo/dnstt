# DNS Scanner — Plan

## Status: IMPLEMENTED (2026-03-29)

All phases completed and integrated into sp-client.

### What was built

| Phase | Commit | What | Status |
|---|---|---|---|
| Phase 1 | `6af6cc2` | scan (stealth/aggressive), validate (delegation + 6 record types + EDNS0), test, prober (DoH+UDP), 51 bundled resolvers, IP exclusion list, probe handler in sp-server | **DONE** |
| Phase 2 | `d0541fd` | bootstrap (--hint dns:IP, site:URL, doh:URL), detect (system DNS, DNS leak technique, transparent proxy, DNS hijack, port reachability, captive portal), hints helper, CDN detection, domain fronting | **DONE** |
| Phase 3 | `5b4f896` | Auto scan mode (network scoring 0-10, OPEN/FILTERED/RESTRICTED/HOSTILE auto-selection), profile (rate limit, fingerprinting, forwarder detection, honeypot, manipulation), monitor (continuous, alerts) | **DONE** |
| Phase 4 | `6ca03ea` | Stealth: DecoyTrafficGenerator (28 popular domains), TrafficShaper (browser-like), QnameCamouflage (CDN-looking names), QueryTypeMixer (A/AAAA/HTTPS/TXT/DNSKEY) | **DONE** |
| Phase 5 | `f5cb6b4` | scan-doh (CT logs crt.sh + subdomain brute-force), scan-dot (16 DoT endpoints, TLS), scan-doq (stub), timing steganography, profile enhancements (cache TTL, DNSSEC, TCP), monitor enhancements (reliability score, adaptive interval, persistent JSON history) | **DONE** |
| Fix | `18b5fdb` | rustls CryptoProvider initialization | **DONE** |
| Phase 9 | `7b47d60` | Integration into sp-client: sp-scanner as library, resolver cache, background scanner (health check 5min, full scan 30min), hot-swap resolvers, network change detection, --auto-discover/--hint/--discovery-only flags | **DONE** |

### CLI commands (12 total)
```
dns-scanner scan          — discover resolvers (auto/stealth/aggressive + --camouflage)
dns-scanner validate      — test delegation + record types + EDNS0
dns-scanner test          — quick reachability test
dns-scanner bootstrap     — auto-discover working path (--hint dns:IP/site:URL/doh:URL)
dns-scanner hints         — help user find hints
dns-scanner detect        — analyze network environment
dns-scanner profile       — deep resolver analysis (rate limit, fingerprint, honeypot)
dns-scanner monitor       — continuous monitoring with alerts + persistent history
dns-scanner scan-doh      — discover new DoH endpoints (CT logs + brute-force)
dns-scanner scan-dot      — discover DoT endpoints (port 853, TLS)
dns-scanner scan-doq      — discover DoQ endpoints (experimental stub)
```

### sp-client integration
```
sp-client --auto-discover                    — enable background scanner + cache
sp-client --discovery-only                   — no config file, auto-discover everything
sp-client --hint dns:10.0.0.1               — bootstrap with known resolver
sp-client --hint doh:https://dns.google/...  — bootstrap with DoH endpoint
sp-client --cache-dir /path                  — persistent resolver cache
```

### Test results (2026-03-29)
- 15 unit tests pass
- scan: 51/52 bundled resolvers alive
- validate: 49/51 delegation works, HTTPS/DNSKEY/NULL/TXT/A/AAAA record types tested
- bootstrap: 22 working paths found, DNS leak discovered 9 real Cloudflare resolvers
- scan-doh: 246 CT log candidates + 416 brute-force → 14 new DoH endpoints
- scan-dot: 15/16 DoT endpoints working
- profile: rate limit, DNSSEC validation, TCP support, fingerprinting confirmed
- monitor: adaptive intervals, persistent JSON history working

---

## Overview

DNS resolver discovery tool for Slipstream Pure tunnel.
Finds ALL open DNS resolvers available from the client's network,
tests delegation to our server, profiles each resolver, monitors reliability.

Pure Rust, cross-platform (Windows/Linux/macOS/Android/iOS).

**Scanner runs on the CLIENT device.** Three modes:
- **auto** (default) — detects network type, chooses stealth or aggressive automatically
- **stealth** — safe for any network, DoH-first, low rate
- **aggressive** (opt-in) — for power users with unrestricted network, full speed UDP scan

Development and testing: both modes tested on client VPS (203.0.113.102).
Production: runs on end-user devices (phones, laptops, etc.)

## Architecture

**Client-side scanner** — runs on the user's device, discovers
what DNS resolvers are reachable from this specific network.

```
Default: dns-scanner scan                     ← stealth mode
Opt-in:  dns-scanner scan --mode aggressive   ← full speed, unrestricted network only
```

## Operational Security

### What the ISP/provider sees

```
DNS queries on port 53:
  - Normal user queries 1-3 DNS servers
  - Scanning hundreds/thousands of IPs on port 53 is SUSPICIOUS
  - ISP may: rate-limit, flag account, alert

DoH queries on port 443:
  - Looks like normal HTTPS to cloudflare.com, google.com, etc.
  - INVISIBLE to ISP — cannot distinguish from normal browsing
  - This is the preferred scanning method
```

### Stealth scanning strategy

```
Rule 1: DoH FIRST, UDP LAST
  - All initial probing through DoH (HTTPS, invisible)
  - UDP port 53 only for validated resolvers, slowly
  - Never mass-scan port 53 from client

Rule 2: Low rate
  - DoH probing: 5-10 requests/sec (looks like browsing)
  - UDP probing: 1-2 requests/sec, max 10-20 resolvers
  - Spread over time, not in bursts

Rule 3: Mix with normal traffic
  - Interleave with real DNS queries (google.com, etc.)
  - ISP sees normal DNS profile with occasional extra queries

Rule 4: Use what the network gives
  - System DNS first (already allowed by network)
  - DoH to known endpoints (invisible)
  - Only probe new UDP resolvers if needed and slowly
```

### What ISP sees with our scanner:
```
Scenario                 | ISP sees                    | Suspicious?
─────────────────────────┼─────────────────────────────┼────────────
Bootstrap (DoH only)     | HTTPS to popular sites      | No
Bootstrap (system DNS)   | DNS to ISP's own server     | No
Test 20 resolvers (DoH)  | 20 HTTPS requests           | No
Test 5 resolvers (UDP)   | 5 DNS queries to known DNS  | No
Full UDP scan 7000 IPs   | 7000 queries to port 53     | YES!
  → we DON'T do this     |                             |
```

### Aggressive mode (--mode aggressive)
> For power users running on a VPS or unrestricted network.
> NOT the default. User explicitly opts in.

```
Differences from stealth:
  Stealth (default)          | Aggressive (opt-in)
  ───────────────────────────┼───────────────────────────────
  DoH first, UDP minimal     | UDP direct to all IPs
  DoH 5-10 rps, UDP 1-2 rps  | 1000-5000+ rps
  Bundled list + public-dns   | Bundled + public-dns + full range scan
  ~1-2 hours background       | ~10-30 minutes
  ISP sees normal traffic     | ISP sees mass port 53 scanning
  Safe for any network        | Only for unrestricted networks

Aggressive mode adds:
  - Scan custom IP ranges: dns-scanner scan --mode aggressive --range 1.0.0.0/8
  - Scan full public-dns.info at high speed
  - Parallel UDP probes (50-100 concurrent)
  - No DoH preference — test everything at once
  - Full IPv4 scan possible: dns-scanner scan --mode aggressive --full-ipv4
    (3.7B IPs, ~20h at 50K pps — USE WITH CAUTION, may trigger ISP alerts)
```

## Phases

### Phase 1: Core (MVP)

#### `dns-scanner scan` — Discover Resolvers from Client Network
- Input: bundled list of known public resolvers (~500 IPs) + public-dns.info list (~7000 IPs)
- For each resolver, two-stage check:
  1. **DoH probe first** (invisible to ISP): if resolver has known DoH URL → test via HTTPS
  2. **UDP probe** (only for resolvers without DoH): DNS A query to port 53, slow rate (2-5 rps)
- Skip: private ranges, multicast, reserved (see Exclusion List)
- Rate control: DoH 5-10 rps (safe), UDP 1-2 rps (stealth)
- Progress display: `[847/7023] doh=312 udp=89 working=76`
- State file: save progress, resume after restart
- Output: `alive.txt` — list of responding IPs

#### `dns-scanner validate` — Delegation + Record Type Test
- Input: alive.txt from scan phase
- Send `scan-<random>.tunnel.example.com` TXT query through each resolver
- Prefer DoH path if available (stealth)
- If TXT response returns with data — delegation works
- 10-20 concurrent workers (not 50-100, stealth)
- Measure latency (3 probes, avg)
- Test EDNS0: send queries with different buffer sizes (512, 1232, 4096)
- Test record types: HTTPS/SVCB, DNSKEY, NULL, TXT, A, AAAA
- Output: `resolvers-validated.toml` (project format, sorted by latency)

#### `dns-scanner test` — Quick Reachability Test
- Input: resolver list (bundled or from previous scan)
- Fast check: which resolvers are reachable RIGHT NOW from this network
- DoH checks first (invisible), then UDP for rest
- 10 concurrent workers, timeout 3s
- Output: `resolvers-local.toml` (ready for client use)

### Phase 2: Encrypted DNS Discovery

#### `dns-scanner scan-doh` — DoH Endpoint Discovery
- Scan known DoH paths on port 443: `/dns-query`, `/resolve`, `/dns`
- Sources:
  - Certificate Transparency logs (crt.sh) — search for domains with `dns`, `doh`, `resolver`
  - Brute subdomain: `dns.`, `doh.`, `resolver.`, `ns.`, `ns1.` on Alexa top-1M domains
  - Known DoH providers list (bundled)
- Send wireformat DoH POST to each candidate
- Validate: response is valid DNS, delegation works
- Output: append to resolvers-validated.toml with `doh = "https://..."` field

#### `dns-scanner scan-dot` — DoT Endpoint Discovery
- Scan port 853 (DNS-over-TLS) on alive IPs
- TLS handshake + DNS query
- Also scan known DoT providers

#### `dns-scanner scan-doq` — DoQ Endpoint Discovery
- Scan port 853/8853 (DNS-over-QUIC)
- QUIC handshake + DNS query

### Phase 3: Network Analysis

#### `dns-scanner detect` — Network Environment Detection
- **Transparent proxy**: send DNS to random non-DNS IP:53, if response comes — proxy detected
- **DNS hijacking**: query for known domain, check if response matches expected IP
- **ISP resolver extraction**: read system DNS config (/etc/resolv.conf, Windows registry), test for delegation
- **Captive portal**: HTTP check to known URLs (captive.apple.com, connectivitycheck.gstatic.com)
- **Port 53 blocking**: try to reach known working resolver on UDP/TCP 53
- **Port 853 blocking**: try DoT to known endpoint
- Report: what works from this network (UDP53, TCP53, DoH443, DoT853)

### Phase 4: Resolver Profiling

#### `dns-scanner profile` — Deep Resolver Analysis
- **Rate limit detection**: gradually increase req/s until drops start, find threshold
- **EDNS0 max size**: binary search for max response size (512..4096)
- **TCP fallback**: test TCP on port 53
- **Response manipulation**: compare response to known-good (detect censorship, ad injection)
- **Cache TTL behavior**: query twice, check if cached, measure cache duration
- **DNSSEC validation**: check if resolver validates DNSSEC (may affect tunnel)
- **Geolocation**: GeoIP lookup for latency estimation
- **Anycast detection**: traceroute-like probe to detect anycast (same IP, different route)
- Update resolvers-validated.toml with profile data

### Phase 5: Long-term Monitoring

#### `dns-scanner monitor` — Continuous Monitoring
- Periodically re-test known resolvers (configurable interval)
- Track: uptime %, avg latency, rate limit changes
- Reliability score (0-100) per resolver
- Adaptive re-scan: unstable resolvers tested more often
- Discover new resolvers in background (slow continuous scan)
- Alert: notify when good resolver goes down or new high-quality one found
- Database: SQLite or simple JSON for history

### Phase 6: Smart Client Bootstrap (Restricted Networks)

#### `dns-scanner bootstrap` — Auto-discovery in Hostile Networks

For clients on networks with DNS whitelists, transparent proxies, or DNS spoofing.
The network MUST have at least one working DNS path (otherwise nothing works at all).
Our job: find that path and check if it forwards TXT queries for tunnel.example.com.

#### Step 1: Network Type Detection
```
1. Read system DNS config
   - Linux: /etc/resolv.conf
   - Windows: registry + ipconfig /all
   - macOS: scutil --dns
   - Android: getprop net.dns1
   - iOS: system resolver API
   → system_dns = [list of IPs]

2. Real resolver discovery (DNS leak technique)
   - Send 5 queries: probe-<random1>.tunnel.example.com ... probe-<random5>.tunnel.example.com
   - Each goes through system DNS → forwarders → real recursive resolver → our server
   - Our server sees SOURCE IP of each query = REAL resolver IP
   - Our server puts source IP in TXT response: "resolver=85.21.192.3"
   - Client receives TXT → now knows the REAL resolver behind any proxies/forwarders
   - 5 probes because ISP often uses pool of resolvers → discover all of them
   - Result: real_resolvers = [85.21.192.3, 85.21.192.4, 85.21.192.7]

   Why 5 probes:
     - ISP recursive resolvers are often a pool (round-robin)
     - Each probe may hit a different server in the pool
     - More probes = more servers discovered = more paths for tunnel

   What we learn:
     system_dns:     10.0.0.1         ← what OS config says (may be forwarder)
     real_resolvers: [85.21.192.3,    ← actual recursive resolvers (from our server)
                      85.21.192.4,
                      85.21.192.7]
     chain:          client → 10.0.0.1 → 85.21.192.3 → our server

   Actions:
     - Test 10.0.0.1 directly (forwarder, may have no rate limit)
     - Test 85.21.192.x directly (real resolver, may be reachable)
     - If 85.21.192.x reachable directly → skip forwarder, lower latency
     - If only 10.0.0.1 reachable → use it (forwarder, still works)

3. Transparent proxy test
   - Send DNS query to random non-DNS IP (e.g. 198.51.100.1:53)
   - If response comes → transparent proxy active
   - The proxy IP doesn't matter — network redirects everything
   - ALSO run probe technique through proxy → discover real resolver behind it

3. DNS hijack test
   - Query A record for known domain (e.g. example.com → 93.184.216.34)
   - If response IP differs → responses being manipulated
   - BUT: hijack may only affect blacklisted domains
   - Our domain tunnel.example.com is unlikely blacklisted

4. Port scan
   - Can we reach 8.8.8.8:53 UDP? → port 53 open
   - Can we reach 8.8.8.8:53 TCP? → TCP fallback available
   - Can we reach 1.1.1.1:443 HTTPS? → DoH available
   - Can we reach 1.1.1.1:853 TLS? → DoT available
```

#### User Hints (--hint flag, fastest path)

User provides what they know about their network. Scanner uses this
to find a working path much faster than blind scanning.

##### Hint type 1: Known DNS resolver IPs

User knows one or more DNS resolver IPs that work in their network
(ISP DNS, corporate DNS, router DNS, etc.)

```
dns-scanner bootstrap --hint dns:10.0.0.1
dns-scanner bootstrap --hint dns:192.168.1.1
dns-scanner bootstrap --hint dns:10.0.0.1,10.0.0.2,172.16.1.1
```

Full workflow:
```
Input: user provides IP(s) they believe are DNS resolvers

Step 1: Alive test (instant)
  ├─ Send simple A query (google.com) to each IP
  ├─ Timeout: 2 seconds
  ├─ Result: which IPs respond to DNS at all
  └─ If none respond → tell user "these IPs don't appear to be DNS servers"

Step 2: Delegation test (instant)
  ├─ Send TXT query: test-<random8>.tunnel.example.com to each alive IP
  ├─ Check: did TXT response come back with tunnel data?
  ├─ If YES → this resolver forwards to our server → USABLE
  ├─ If NO (NXDOMAIN/empty) → resolver doesn't forward
  │   ├─ Maybe it's a forwarder that blocks unknown TLDs
  │   ├─ Maybe it only resolves internal domains
  │   └─ Report: "resolver alive but doesn't forward to our server"
  └─ If NO (timeout) → resolver blocks our query type
      └─ Try TXT → then A → then HTTPS record type

Step 3: Deep probe (for each working resolver)
  ├─ Rate limit test: send 10, 20, 50, 100 queries/sec
  │   └─ Find max rps without drops (ISP DNS often 200+ rps!)
  ├─ Record type test: TXT, HTTPS, DNSKEY, NULL, A, AAAA
  │   └─ Find which types pass through intact
  ├─ EDNS0 size test: 512, 1232, 2048, 4096
  │   └─ Find max response size
  ├─ Latency: 10 probes, calculate avg/min/max/jitter
  └─ Forwarder detection:
      ├─ Query for resolver's own NS records
      ├─ Check: is this a forwarding DNS (dnsmasq, router) or recursive?
      ├─ If forwarder → what upstream does it use?
      │   (may discover additional resolvers we can use directly)
      └─ Forwarders add latency but often have no rate limit

Step 4: Report
  ├─ For each IP:
  │   status: working / alive-no-delegation / dead
  │   latency: 12ms
  │   rate_limit: 250 rps (!)
  │   best_record_type: HTTPS
  │   edns0_max: 4096
  │   type: forwarder → upstream 8.8.8.8
  └─ Auto-generate resolvers-local.toml

Total time: 5-15 seconds for 1-5 IPs

Why this is the BEST path:
  - ISP/corporate DNS = no rate limit for local clients
  - Google limits to ~30 rps, ISP DNS may allow 200+ rps
  - 200 rps × 1000 bytes = 200 KB/s through ONE resolver
  - vs 30 rps × 1000 bytes = 30 KB/s through Google
  - 6-7x speed improvement!
```

##### Hint type 2: Known working website URLs

User knows websites that open in their browser (whitelisted sites).
Scanner extracts DNS path and CDN info from these.

```
dns-scanner bootstrap --hint site:https://bank.ru
dns-scanner bootstrap --hint site:https://mail.google.com
dns-scanner bootstrap --hint site:https://vk.com,https://yandex.ru,https://bank.ru
```

Full workflow:
```
Input: user provides URL(s) of sites that work in their network

Step 1: Resolve the domain + discover REAL resolver (DNS leak technique)
  ├─ Resolve bank.ru using system DNS
  ├─ If resolves → system DNS works for at least some domains
  ├─ SIMULTANEOUSLY: send probe-<random>.tunnel.example.com through system DNS
  │   → our server sees SOURCE IP of real recursive resolver
  │   → returns it in TXT: "resolver=85.21.192.3"
  │   → client now knows the REAL resolver, not just forwarder
  ├─ Send 5 probes to discover resolver pool:
  │   real_resolvers = [85.21.192.3, 85.21.192.4, 85.21.192.7]
  ├─ Test each for delegation:
  │   - System DNS (10.0.0.1) → TXT test-XXXX.tunnel.example.com → works?
  │   - Real resolver (85.21.192.3) → try direct → works? lower latency?
  │   - Real resolver (85.21.192.4) → try direct → works?
  ├─ Result: we know BOTH the forwarder AND real resolvers
  │   - Forwarder: no rate limit, higher latency
  │   - Real resolver direct: may have rate limit, lower latency
  │   - Choose best combination
  └─ Even if system DNS has blocklist, our domain may not be in it

Step 2: Detect CDN (for domain fronting)
  ├─ Connect HTTPS to the site, check:
  │   - TLS certificate: issued by? (Cloudflare, etc.)
  │   - HTTP headers: Server, CF-Ray, X-CDN, Via
  │   - IP range: belongs to which CDN? (whois/ASN lookup)
  │
  ├─ CDN detection results:
  │   bank.ru → Cloudflare (CF-Ray header, IP in 104.x.x.x)
  │   mail.google.com → Google (IP in 142.250.x.x)
  │   vk.com → Own infra (no CDN)
  │
  └─ CDN → DoH mapping:
     CDN              | DoH behind same infra          | Domain fronting
     ─────────────────┼────────────────────────────────┼─────────────────
     Cloudflare       | cloudflare-dns.com/dns-query   | YES — best option
     Google           | dns.google/dns-query           | Limited
     Amazon CloudFront| —                              | Possible with custom
     Fastly           | —                              | No native DoH
     Akamai           | —                              | No native DoH
     Yandex CDN       | dns.yandex.ru (not standard DoH)| No
     VK CDN           | —                              | No

Step 3: Domain fronting (if CDN supports it)
  ├─ Example: bank.ru is on Cloudflare
  ├─ Build HTTPS request:
  │   TLS SNI = bank.ru          ← DPI sees whitelisted domain
  │   Host header = cloudflare-dns.com  ← Cloudflare routes to DoH
  │   Path = /dns-query
  │   Body = DNS wire format TXT query for test-XXXX.tunnel.example.com
  │
  ├─ Send request
  ├─ If valid DNS response with TXT data → DOMAIN FRONTING WORKS
  │   → This is the most stealth path possible
  │   → DPI sees HTTPS to bank.ru (whitelisted)
  │   → Actually talking to Cloudflare DNS
  │   → Tunnel data flows through
  │
  └─ If not on supported CDN → domain fronting not possible for this site
     → But Step 1 may have found system DNS works

Step 4: Extract additional resolver info from site
  ├─ HTTP headers may reveal internal infra:
  │   - X-DNS-Prefetch-Control → site uses DNS prefetch
  │   - Alt-Svc → HTTP/3 endpoints (may be DoQ-capable)
  │   - Public-Key-Pins → cert info
  ├─ DNS records of the site itself:
  │   - NS records → who runs DNS for this domain
  │   - MX records → mail infrastructure
  │   - These NS/MX servers are DNS resolvers too → test them
  └─ Subdomains with DNS-like names:
     dns.bank.ru, ns1.bank.ru, resolver.bank.ru
     → May be internal resolvers accessible from this network

Step 5: Report
  ├─ For each site:
  │   domain: bank.ru
  │   cdn: Cloudflare
  │   fronting: YES → cloudflare-dns.com/dns-query via SNI=bank.ru
  │   system_dns: 10.0.0.1 → delegation WORKS
  │   ns_servers: ns1.bank.ru (185.x.x.x) → delegation NO
  │   discovered_resolvers: [10.0.0.1]
  └─ Auto-generate resolvers-local.toml with best paths

Total time: 3-10 seconds per site
```

##### Hint type 3: Known DoH endpoint URL

User knows a DoH URL that works from their network.

```
dns-scanner bootstrap --hint doh:https://dns.company.com/dns-query
dns-scanner bootstrap --hint doh:https://dns.google/dns-query
```

Full workflow:
```
Step 1: Send TXT test-XXXX.tunnel.example.com via DoH POST → check response
Step 2: If works → profile (latency, rate limit, record types, EDNS0)
Step 3: DONE — use this endpoint
Total time: 1-2 seconds
```

##### Multiple hints (test all in parallel)

```
dns-scanner bootstrap \
  --hint dns:10.0.0.1 \
  --hint dns:192.168.1.1 \
  --hint site:https://bank.ru \
  --hint site:https://vk.com \
  --hint doh:https://dns.google/dns-query

Workflow:
  1. Launch ALL hint checks in parallel (tokio tasks)
  2. As each completes → add to results
  3. After all done (or timeout 15s) → rank by:
     - Working? (delegation confirmed)
     - Speed (latency)
     - Rate limit (higher = better for tunnel)
     - Stealth (DoH/fronting > UDP)
  4. Auto-select best combination:
     - Primary: fastest working path
     - Backup: second best (different type for redundancy)
     - Stealth: best domain-fronting path (if available)
  5. Generate resolvers-local.toml

Typical result:
  PRIMARY: dns:10.0.0.1 (12ms, 200 rps, ISP resolver)     ← fastest
  BACKUP:  doh:dns.google (45ms, 30 rps, DoH)              ← most reliable
  STEALTH: site:bank.ru→fronting (80ms, 30 rps, Cloudflare) ← most hidden
```

##### Hint discovery helper

User doesn't know what to provide? Scanner helps:

```
dns-scanner hints

Output:
  Detected system DNS: 10.0.0.1, 10.0.0.2
  → try: dns-scanner bootstrap --hint dns:10.0.0.1

  Router/gateway: 192.168.1.1
  → try: dns-scanner bootstrap --hint dns:192.168.1.1

  Your browser history suggests these sites work:
  (not implemented — manual input for now)
  → try: dns-scanner bootstrap --hint site:https://example.com

  Known DoH endpoints to try:
  → try: dns-scanner bootstrap --hint doh:https://dns.google/dns-query
```

CDN Detection for Domain Fronting:
```
CDN              | DoH endpoint behind it          | Fronting works?
─────────────────┼─────────────────────────────────┼────────────────
Cloudflare       | cloudflare-dns.com/dns-query    | Yes (same infra)
Google Cloud CDN | dns.google/dns-query            | Limited
Fastly           | —                               | No native DoH
Akamai           | —                               | No native DoH
AWS CloudFront   | doh.cleanbrowsing.org possible  | Depends on config
Yandex CDN       | dns.yandex.ru                   | Non-standard
```

If user's whitelisted site is on Cloudflare → we can domain-front through it
to reach cloudflare-dns.com DoH. DPI sees HTTPS to allowed-site.com, never
sees DNS traffic.

#### Step 2: Find Working Paths (priority order)

```
Path A: DoH (port 443) — MOST RELIABLE
  ├─ Try known DoH endpoints:
  │   Cloudflare: https://cloudflare-dns.com/dns-query
  │   Google: https://dns.google/dns-query
  │   Quad9: https://dns.quad9.net/dns-query
  │   NextDNS: https://dns.nextdns.io/
  │   AdGuard: https://dns.adguard-dns.com/dns-query
  │   + all from bundled list + master list from VPS
  ├─ Send TXT query for test-XXXX.tunnel.example.com
  ├─ If TXT response with data → PATH WORKS
  └─ HTTPS almost never blocked (breaks too many things)

Path B: System/Forced DNS (whatever the network gives us)
  ├─ Use system_dns from Step 1
  ├─ Send TXT query for test-XXXX.tunnel.example.com via UDP
  ├─ If TXT response with data → PATH WORKS
  ├─ Key insight: even censoring DNS usually forwards TXT
  │   for unknown domains (tunnel.example.com not in blocklists)
  └─ This works even with transparent proxy!

Path C: Transparent Proxy
  ├─ If proxy detected in Step 1
  ├─ Send TXT query for test-XXXX.tunnel.example.com to ANY IP:53
  ├─ Proxy intercepts and resolves recursively
  ├─ If TXT response with data → PATH WORKS
  └─ Doesn't matter what IP we send to — proxy catches all

Path D: DoT (port 853)
  ├─ Try known DoT endpoints
  ├─ TLS handshake + TXT query
  └─ Less likely to work than DoH, but worth trying

Path E: DoQ (port 8853)
  ├─ Try known DoQ endpoints
  ├─ QUIC handshake + DNS query
  └─ Newest protocol, few blocklists know about it

Path F: Domain Fronting (last resort)
  ├─ DoH request to CDN domain (looks like normal HTTPS)
  ├─ SNI = allowed-domain.com, Host header = dns-endpoint
  ├─ Works even with deep packet inspection on SNI
  ├─ CDN providers: Cloudflare, Fastly, Akamai
  └─ Requires CDN that supports DNS and doesn't block fronting

Path G: DNS-over-HTTPS via generic HTTPS proxy
  ├─ If network has HTTPS proxy (corporate environments)
  ├─ CONNECT to DoH endpoint through proxy
  └─ Looks like normal HTTPS traffic to proxy
```

#### Step 3: Optimize Found Paths
```
For each working path:
  - Measure latency (3 probes)
  - Test rate limit (gradually increase req/s)
  - Test EDNS0 max response size
  - Score: latency × reliability × bandwidth

Select top N paths for tunnel use.
Prefer multiple paths for redundancy.
```

#### Step 4: Continuous Adaptation
```
While tunnel running:
  - Re-test working paths every 5 min
  - If path dies → failover to backup
  - Background: slowly try other paths (new DoH, new resolvers)
  - If network changes (new DHCP, new WiFi) → re-run bootstrap
```

#### Network Types vs Working Paths
```
Network Type          | Likely Working Paths
─────────────────────-┼──────────────────────────────────
Open (no restrictions) | All paths (UDP53, DoH, DoT, DoQ)
Port 53 whitelist      | DoH(443), DoT(853), system DNS
Transparent DNS proxy  | DoH(443), proxy path, DoT(853)
DNS spoofing (partial) | DoH(443), system DNS (TXT ok), DoT
Full DPI + blocking    | Domain fronting, HTTPS proxy
Corporate firewall     | DoH via HTTPS proxy, system DNS
Mobile carrier         | DoH(443), carrier DNS (usually recursive)
China/Iran GFW         | Domain fronting, DoH with ECH, DoQ
```

### Phase 7: Record Type Probing & Adaptive Encoding

#### Problem
TXT records are suspicious for DPI. Different resolvers support different
record types. The scanner must determine the best encoding per resolver.

#### Downstream Record Types (server → client)

High capacity + low suspicion:
```
Type         | Capacity  | Suspicion | Why safe
─────────────┼───────────┼───────────┼──────────────────────────────────
HTTPS/SVCB   | ~900 bytes| Very low  | Every browser sends thousands/day, DPI can't filter yet
  (type 65)  |           |           | Key-value pairs, easy to encode data
DNSKEY       | ~900 bytes| Very low  | DNSSEC record, large binary expected
             |           |           | Blocking DNSKEY breaks all DNSSEC → nobody does it
RRSIG        | ~900 bytes| Very low  | DNSSEC signature, always large binary blobs
NULL         | ~900 bytes| Very low  | Almost unknown type, DPI skips it
             |           |           | Risk: some resolvers may not forward NULL queries
CERT         | ~900 bytes| Low       | Certificate record, arbitrary binary
OPENPGPKEY   | ~900 bytes| Low       | PGP key, binary, expected to be large
TLSA         | ~900 bytes| Low       | TLS cert association, binary
SSHFP        | ~900 bytes| Low       | SSH fingerprint, binary
SMIMEA       | ~900 bytes| Low       | S/MIME cert, binary
TXT          | ~900 bytes| HIGH      | Classic tunnel indicator, DPI flags this
```

Medium capacity:
```
Type         | Capacity  | Description
─────────────┼───────────┼──────────────────────────────────
NAPTR        | ~400 bytes| Many text fields (flags, services, regexp)
URI          | ~400 bytes| priority + weight + URI string
HINFO        | ~500 bytes| CPU + OS, two text fields
SRV          | ~250 bytes| priority + weight + port + target
CAA          | ~250 bytes| flags + tag + value
MX           | ~200 bytes| priority + domain name
CNAME        | ~200 bytes| single domain name
DNAME        | ~200 bytes| like CNAME but for zone
PTR          | ~200 bytes| domain name
```

Low capacity but invisible:
```
Type         | Capacity       | Description
─────────────┼────────────────┼──────────────────────────────────
A            | 4 bytes × N    | Multiple A records, looks like CDN/load balancer
AAAA         | 16 bytes × N   | IPv6, more data per record
             |                | Impossible to distinguish from normal DNS
```

#### Upstream Record Types (client → server)

```
Method                    | Capacity     | Suspicion
──────────────────────────┼──────────────┼──────────────────
QNAME subdomains (current)| ~100 bytes   | Medium (long names)
EDNS0 OPT custom (current)| ~1200 bytes  | Low
EDNS0 Padding (RFC 7830)  | ~400 bytes   | Very low (standard option)
EDNS0 Client Subnet       | ~16 bytes    | Very low (normal option)
EDNS0 Cookie (RFC 7873)   | ~32 bytes    | Very low (normal option)
Short QNAME + padding     | Varies       | Very low
```

#### Best Choices (priority order)

```
1. HTTPS/SVCB (type 65) — BEST CHOICE
   - New type, every browser sends thousands of these daily
   - DPI hasn't learned to filter yet
   - Key-value pairs → easy data encoding
   - High capacity
   - Looks like: "browser asking HTTPS record for a website"

2. DNSKEY/RRSIG — DNSSEC camouflage
   - DNSSEC is everywhere, expected traffic
   - Large binary data is normal for these types
   - DPI blocking DNSKEY would break all DNSSEC → nobody does this
   - Looks like: "resolver verifying DNSSEC signature"

3. NULL — stealth
   - Almost nobody knows about this type
   - DPI just passes unknown types through
   - Risk: some resolvers may not forward NULL queries

4. A/AAAA multiple — maximum camouflage
   - 10 A records = 40 bytes, slow but IMPOSSIBLE to distinguish
   - Looks like normal CDN DNS response
   - Last resort when everything else is filtered
```

#### `dns-scanner validate --record-types`

For each resolver, test:
```
1. Send query for each record type (TXT, NULL, DNSKEY, HTTPS, RRSIG, A, AAAA...)
2. Check: did the response arrive intact?
3. Check: was the response modified/stripped?
4. Check: max response size for each type
5. Measure: latency difference per type (some types may be slower)

Output per resolver:
  resolver: 8.8.8.8
  supported_types:
    - HTTPS: ok, max=900, latency=45ms    ← BEST
    - DNSKEY: ok, max=900, latency=47ms
    - TXT: ok, max=900, latency=44ms
    - NULL: blocked                        ← resolver drops NULL
    - A: ok, max=40 (10 records), latency=42ms
    - AAAA: ok, max=160 (10 records), latency=43ms
```

Client selects best type per resolver:
- HTTPS/SVCB if supported → max camouflage + capacity
- DNSKEY if DNSSEC camouflage needed
- NULL if resolver passes it through
- TXT as fallback (works everywhere)
- A/AAAA as last resort (slow but invisible)

#### Resolver Blocking Bypass

```
Problem                   | Bypass
──────────────────────────┼──────────────────────────────────
Domain in blocklist       | Backup domains on different TLDs (future)
TLD blocked               | Domain on popular TLD (.com, .net) (future)
TXT stripped/filtered     | Use HTTPS/DNSKEY/NULL instead
DNSSEC rejection          | Sign zone with DNSSEC
Rate limit per-domain     | Subdomain rotation (rand1.tunnel.example.com)
Firewall to our server    | Anycast — multiple servers (future)
Large responses cut       | Adaptive batch size, TCP fallback
Record type blocked       | Auto-switch to next supported type
```

### Phase 8: Advanced Discovery

#### 8a. API-based Discovery (FUTURE — implement after tunnel is stable)
> Using third-party scan databases instead of scanning ourselves.
> Implement after full tunnel is working and tested.
> Adds external API dependencies, requires API keys.

```
Shodan: API query "port:53 protocol:dns" → millions of IPs
Censys: search for open DNS resolvers
FOFA: Chinese alternative to Shodan

dns-scanner discover --source shodan --api-key XXX
dns-scanner discover --source censys
dns-scanner discover --source all

Pros: instant results, no scanning time, fresh data
Cons: API keys needed, paid for full results, third party sees our queries
```

#### 8b. P2P Resolver Sharing (FUTURE — not in current scope)
> Clients share discovered resolvers with each other through the tunnel.
> Implement after tunnel is stable and has multiple users.

```
How it works:
  - Client A finds 500 working resolvers
  - Client B finds 300 different ones
  - Both upload their lists to the server (encrypted)
  - Server merges, deduplicates, validates
  - New clients get the merged list on connect
  - More users = more resolvers = better for everyone

Protocol:
  - Client → server: RESOLVER_SHARE message with list
  - Server validates each resolver (delegation test)
  - Server → client: RESOLVER_LIST message with merged list
  - Include trust score: resolver found by N clients = more trustworthy

Privacy:
  - Don't share which client found which resolver
  - Don't share client IP or location
  - Only share: resolver IP, supported types, latency range
```

#### 8c. Traffic Camouflage (Anti-fingerprinting)

DPI can detect DNS tunnels not by record type but by **traffic patterns**:
- Too many queries to one domain
- Unusually long QNAMEs
- Suspiciously regular intervals between queries
- High ratio of TXT queries (normal users rarely query TXT)

```
Traffic Shaping — mimic browser DNS patterns:
  - Randomize intervals (not steady 50ms, but 30-80ms jitter)
  - Burst like a browser (page load → 20 queries, then silence)
  - Vary query rate over time (not constant throughput)

Decoy Traffic — mix in legitimate DNS queries:
  - 30% of traffic = real DNS queries to popular sites
  - 70% = tunnel queries
  - DPI sees normal user DNS profile
  - Decoy list: top-1000 Alexa domains, randomized

QNAME Camouflage:
  - Instead of: base32.base32.base32.tunnel.example.com
  - Look like:  cdn-cache-a7f3.static.tunnel.example.com
  - Or:         img-001.media.tunnel.example.com (looks like CDN)
  - Or:         v2.api.tunnel.example.com (looks like API call)
  - Pattern: <word>-<hex>.<word>.tunnel.example.com

Query Type Mixing:
  - Don't send 100% DNSKEY queries (suspicious)
  - Mix: 40% A + 30% AAAA + 20% HTTPS + 10% DNSKEY
  - Each type carries data, but the mix looks natural
```

#### 8d. Resolver Fingerprinting

```
Identify resolver software to predict behavior:

Software    | Detection method              | Behavior
────────────┼───────────────────────────────┼────────────────────────
BIND        | NSID, version.bind CHAOS      | Predictable rate limit, good EDNS0
Unbound     | NSID, response header pattern | Strict DNSSEC, good EDNS0
PowerDNS    | NSID, specific flags          | Configurable, good performance
dnsmasq     | Small cache, specific errors  | Small cache, may truncate large responses
Knot        | NSID, header flags            | Fast, good EDNS0
Windows DNS | Response pattern              | Corporate, may filter
Cloudflare  | Anycast, specific headers     | Fast, good EDNS0, rate limit ~50rps
Google      | Known IPs, EDNS client subnet | Good EDNS0, rate limit ~30rps

Why useful:
  - Know rate limit before hitting it
  - Know max response size
  - Predict which record types work
  - Avoid problematic resolver configurations
```

#### 8e. Honeypot Detection

```
Some "resolvers" are traps that log tunnel traffic:

Detection methods:
  1. Query non-existent domain → if resolves → honeypot or NXDOMAIN hijacker
  2. Compare responses with trusted resolver (Google) → if different → manipulation
  3. Too good to be true? (0ms latency, unlimited rate, 100% uptime) → suspicious
  4. Behavior change: works perfectly for 100 queries, then blocks → monitoring
  5. Check IP reputation: is this IP known as a honeypot/sinkhole?
  6. Reverse DNS: does resolver IP resolve to security-company domain?

Action:
  - Flag suspicious resolvers, don't use for tunnel
  - Monitor behavior over time, downgrade score if changes
  - Never send real tunnel data through unvalidated resolver first time
    → send test data, verify round-trip, then promote to active
```

#### 8f. Non-standard DNS Channels

```
DNS-over-WebSocket (DoW):
  - WebSocket on port 443, looks like normal web traffic
  - Some CDN/proxies pass WebSocket through
  - Protocol: WS connection → send DNS wire format frames
  - Advantage: persistent connection, less overhead than DoH per-query

DNS-over-HTTP/2 multiplexed:
  - Multiple DNS queries in single HTTP/2 connection
  - Less overhead than individual DoH queries
  - Advantage: connection reuse, server push potential

DNS-over-HTTP/3 (QUIC):
  - DoH but over QUIC/HTTP3
  - 0-RTT connection setup
  - Better for mobile (connection migration)
```

#### 8g. Distributed Multi-VPS Scanning (FUTURE — implement after tunnel is stable)
> Multiple VPS in different countries scan in parallel.
> Implement after full tunnel is working and tested.
> Requires multiple VPS and coordination protocol.

```
How it would work:
  - VPS-1 (EU): scans European IP ranges
  - VPS-2 (US): American ranges
  - VPS-3 (Asia): Asian ranges
  - Results merge on central server
  - Full IPv4 scan in 4-5 hours instead of 20

Coordination:
  - Central server assigns IP ranges to scanners
  - Scanners report alive IPs back
  - Central server runs validation
  - Deduplication + merge into master list
```

#### 8h. Timing Steganography

```
Ultra-covert mode (very slow, but INVISIBLE):
  - Data encoded NOT in query content
  - But in intervals BETWEEN queries
  - 50ms gap = bit 0, 100ms gap = bit 1
  - DPI CANNOT detect: queries are completely normal
  - Speed: ~10 bits/sec (1.25 bytes/sec)

Use cases (too slow for tunnel, but good for):
  - Initial bootstrap signaling
  - Key exchange
  - Command channel (tell client to switch resolver/domain)
  - Heartbeat with embedded status
```

### Server-side: Probe Response Protocol

The DNS leak technique requires our server (sp-server) to support
probe queries. This is a small addition to the server code.

#### Probe query format
```
Query:   probe-<random>.tunnel.example.com  TXT
         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^
         "probe-" prefix = scanner probe, not tunnel data

Server behavior:
  1. Detect "probe-" prefix in subdomain
  2. Log source IP of the DNS query (= real recursive resolver IP)
  3. Return TXT response:
     "resolver=<source_ip>;time=<unix_ts>;probe=<random>"

Example:
  Client sends:     probe-a7f3c9.tunnel.example.com TXT
  via system DNS:   10.0.0.1 (forwarder)
  real resolver:    85.21.192.3 (recursive, queries our server)
  Server receives query from: 85.21.192.3
  Server responds:  TXT "resolver=85.21.192.3;time=1711670400;probe=a7f3c9"
  Client receives:  now knows real resolver = 85.21.192.3
```

#### Multiple probes = discover resolver pool
```
probe-aaa.tunnel.example.com �� resolver=85.21.192.3
probe-bbb.tunnel.example.com → resolver=85.21.192.4  ← different server!
probe-ccc.tunnel.example.com → resolver=85.21.192.3
probe-ddd.tunnel.example.com → resolver=85.21.192.7  ← another one!
probe-eee.tunnel.example.com → resolver=85.21.192.4

Result: ISP resolver pool = [85.21.192.3, 85.21.192.4, 85.21.192.7]
  → 3 resolvers discovered through ONE system DNS entry
  → Each can be used as separate tunnel path
```

#### Record type probes
```
Server also supports probe queries for different record types:

probe-XXX.tunnel.example.com A       → A record with encoded resolver IP
probe-XXX.tunnel.example.com AAAA    → AAAA record with encoded data
probe-XXX.tunnel.example.com HTTPS   → HTTPS/SVCB record with resolver info
probe-XXX.tunnel.example.com DNSKEY  → DNSKEY record with resolver info
probe-XXX.tunnel.example.com NULL    → NULL record with resolver info

If client receives valid response for a type →
  that record type passes through this resolver intact.
```

#### Implementation in sp-server
```rust
// In server query handler, before tunnel logic:
if subdomain.starts_with("probe-") {
    let source_ip = query_source_addr.ip();
    let probe_id = subdomain.strip_prefix("probe-").unwrap();
    let response = format!("resolver={};time={};probe={}",
        source_ip, unix_timestamp(), probe_id);
    return build_txt_response(query, &response);
}
```

Small change to server, big value for scanner.

### Phase 9: Integration

#### Client auto-discovery
- Embed scanner logic into sp-client
- On startup: load cached resolvers + run quick test
- Background: slow continuous discovery
- Hot-swap: add new resolvers to good paths without restart
- Persistent cache file between restarts

## Full Architecture Overview

```
Level 0: Discovery (find resolvers — CLIENT SIDE ONLY)
  ├─ Bundled list + public-dns.info probe (Phase 1)
  ├─ DoH/DoT/DoQ endpoint discovery (Phase 2)
  ├─ Bootstrap: system DNS, transparent proxy (Phase 6)
  ├─ [future] P2P sharing between clients (Phase 8b)
  ├─ [future] API: Shodan/Censys/FOFA (Phase 8a)
  └─ [future] Distributed multi-VPS scanning (Phase 8g)

Level 1: Validation (test delegation + record types)
  ├─ Delegation test (Phase 1)
  ├─ Record type probing (Phase 7)
  ├─ Resolver fingerprinting (Phase 8d)
  └─ Honeypot detection (Phase 8e)

Level 2: Network Analysis (understand restrictions)
  ├─ Bootstrap + detect (Phase 6)
  ├─ User hints (Phase 6)
  └─ Transparent proxy / hijack detection (Phase 3)

Level 3: Stealth (camouflage traffic)
  ├─ Adaptive record type selection (Phase 7)
  ├─ Traffic shaping + decoy traffic (Phase 8c)
  ├─ QNAME camouflage (Phase 8c)
  ├─ Domain fronting (Phase 6)
  └─ Timing steganography (Phase 8h)

Level 4: Optimization (maximize speed)
  ├─ Per-resolver profiling (Phase 4)
  ├─ Best record type per resolver (Phase 7)
  ├─ Multi-resolver parallel streams
  └─ Monitoring + failover (Phase 5)
```

## Output Format

### resolvers-validated.toml (project format)
```toml
[[resolver]]
name = "8.8.8.8"
ip = ["8.8.8.8"]
doh = "https://dns.google/dns-query"  # optional
ech = false
latency_ms = 45
edns0_max = 4096
rate_limit_rps = 30
reliability = 95
last_seen = "2026-03-29T12:00:00Z"
```

## Scan Strategy

### Client-side scanning approach
Scanner runs on client, not VPS. Must be stealthy.

```
Step 1: Bundled known resolvers (~500 IPs with DoH URLs)
  → Test via DoH first (invisible to ISP)
  → 5-10 requests/sec, looks like normal browsing
  → Expected result: 50-200 working resolvers

Step 2: Download public-dns.info list (~7000 IPs)
  → Filter: skip IPs already tested in Step 1
  → Test via DoH where URL known, UDP for rest
  → UDP rate: 1-2 rps (stealth), spread over ~1 hour
  → Expected result: 200-500 additional working resolvers

Step 3: System DNS + network-specific discovery
  → Test ISP's resolver (from system config)
  → Transparent proxy detection
  → These are often the fastest (no rate limit for local clients)

Total time: ~1-2 hours for full scan (background, non-blocking)
Quick bootstrap: ~10 seconds (test top-20 known DoH endpoints)
```

### Randomize probe order
- Shuffle the list before scanning
- Don't probe sequentially (1.1.1.1, 1.0.0.1, 4.2.2.1...)
- Randomize to avoid pattern detection

### VPS-side IP scanning (NOT IMPLEMENTED — reference only)
> For scanning the full IPv4 space (3.7B IPs) from a VPS:
> - Fisher-Yates shuffle too much memory for 3.7B IPs
> - Better: encrypt sequential counter with AES-128 → pseudo-random permutation
> - Same seed = same order = resumable
> - Priority ranges first: known DNS ASNs, cloud providers, ISPs, universities
> - Rate: 10,000-50,000 pps, full scan in 20h-4 days
> We don't implement this — client-side discovery is sufficient.

### Exclusion List
- 0.0.0.0/8 — current network
- 10.0.0.0/8 — private
- 100.64.0.0/10 — CGNAT
- 127.0.0.0/8 — loopback
- 169.254.0.0/16 — link-local
- 172.16.0.0/12 — private
- 192.0.0.0/24 — IETF protocol
- 192.0.2.0/24 — documentation
- 192.168.0.0/16 — private
- 198.18.0.0/15 — benchmarking
- 198.51.100.0/24 — documentation
- 203.0.113.0/24 — documentation
- 224.0.0.0/4 — multicast
- 240.0.0.0/4 — reserved
- 255.255.255.255/32 — broadcast

## CLI Usage

```
# Quick bootstrap — find working path in seconds
dns-scanner bootstrap                                        # auto-detect everything
dns-scanner bootstrap --hint dns:10.0.0.1                    # known DNS resolver
dns-scanner bootstrap --hint dns:10.0.0.1,10.0.0.2          # multiple resolvers
dns-scanner bootstrap --hint site:https://bank.ru            # working whitelisted site
dns-scanner bootstrap --hint site:https://bank.ru,https://vk.com  # multiple sites
dns-scanner bootstrap --hint doh:https://dns.google/dns-query     # known DoH endpoint
dns-scanner bootstrap --hint dns:10.0.0.1 --hint site:https://bank.ru  # combine all
dns-scanner hints                                            # help user find hints

# Full scan — stealth mode (default, safe for any network)
dns-scanner scan --output alive.txt
dns-scanner scan --resume state.json                         # resume interrupted scan
dns-scanner scan --udp-rate 2 --doh-rate 10                  # custom rate limits

# Full scan — aggressive mode (opt-in, unrestricted network only)
dns-scanner scan --mode aggressive --output alive.txt
dns-scanner scan --mode aggressive --range 1.0.0.0/8         # scan specific range
dns-scanner scan --mode aggressive --full-ipv4               # scan ALL IPv4 (caution!)

# Validate found resolvers (delegation + record types)
dns-scanner validate --input alive.txt --domain tunnel.example.com --output resolvers.toml
dns-scanner validate --record-types --input resolvers.toml   # test all record types

# Quick reachability test
dns-scanner test --input resolvers.toml --output resolvers-local.toml

# DoH/DoT/DoQ discovery
dns-scanner scan-doh --output doh-found.toml
dns-scanner scan-dot --output dot-found.toml

# Network analysis
dns-scanner detect

# Profile resolvers
dns-scanner profile --input resolvers.toml --output resolvers-profiled.toml

# Monitor
dns-scanner monitor --input resolvers.toml --interval 600
```

## Dependencies
- tokio — async runtime
- hickory-proto — DNS wire format (already in project)
- clap — CLI args
- serde/toml — config serialization
- rand — IP randomization
- reqwest — DoH HTTP requests (already in project)
- rustls — DoT TLS (already in project via quinn)

## Implementation Order

### DONE — Must have (Phase 1-3)
1. ~~Bundled resolver list + public-dns.info loader~~ ✓
2. ~~DoH prober (stealth, primary method)~~ ✓
3. ~~UDP prober (low-rate, secondary method)~~ ✓
4. ~~Delegation validator (validate)~~ ✓
5. ~~Record type probing (HTTPS, DNSKEY, NULL, TXT, A/AAAA)~~ ✓
6. ~~Quick reachability tester (test)~~ ✓
7. ~~State file + resume~~ ✓
8. ~~Progress display~~ ✓

### DONE — Important (Phase 4-7)
9. ~~Bootstrap: network detection + path probing + user hints~~ ✓
10. ~~DoH endpoint discovery (scan-doh)~~ ✓
11. ~~DoT/DoQ scanner~~ ✓
12. ~~Domain fronting fallback~~ ✓
13. ~~Profiling (rate limit, EDNS0, manipulation, fingerprinting)~~ ✓
14. ~~Honeypot detection~~ ✓
15. ~~Monitoring + reliability tracking~~ ✓

### DONE — Stealth (Phase 8c)
16. ~~Traffic shaping (browser pattern mimicry)~~ ✓
17. ~~Decoy traffic (mix real DNS queries)~~ ✓
18. ~~QNAME camouflage~~ ✓
19. ~~Timing steganography (ultra-covert command channel)~~ ✓

### DONE — Integration (Phase 9)
20. ~~Client integration (embed in sp-client, auto-discover, cache, hot-swap)~~ ✓
21. ~~Continuous adaptation (background scanner, network change detection)~~ ✓

### Future (implement after tunnel is stable)
- [future] P2P resolver sharing between clients (Phase 8b)
- [future] API-based discovery: Shodan/Censys/FOFA (Phase 8a)
- [future] Distributed multi-VPS scanning (Phase 8g)
- [future] VPS-side full IPv4 scanning (Operational Security section)
- [future] Full DoQ protocol support via quinn (Phase 2, currently stub)
- [future] DNS-over-WebSocket, HTTP/2 multiplex, HTTP/3 (Phase 8f)
