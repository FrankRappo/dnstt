# Diagnosing whether DNS packets actually reach the tunnel server

A short, reproducible method to answer two different questions that are easy to confuse:

1. Is the tunnel server alive?
2. Do packets from **this** client network reach it?

`dig` alone cannot answer the second one, because a network in the path may answer on the
server's behalf.

## Method

On the server:

```bash
nohup timeout 900 tcpdump -i any -n -l -tttt port 53 > /opt/slipstream/tmp/capture53.log 2>&1 &
```

On the client, in this order:

```bash
dig example.com @203.0.113.130          # the tunnel server (UDP)
dig +tcp example.com @203.0.113.130     # same over TCP
dig example.com @192.0.2.1              # TEST-NET-1: nobody can answer
dig example.com @198.51.100.7           # TEST-NET-2: nobody can answer
```

The TEST-NET probes are the key. RFC 5737 ranges are reserved for documentation and are not
routed on the public internet, so **any answer from them means the packet never left the local
network** — something in the path (router "DNS assistant", ISP transparent resolver, proxy)
answered it.

Then compare with the capture: every packet that truly reached the server appears as an
inbound line from the client's public address.

## Observed results (two independent runs, three weeks apart)

| Check | Result |
|---|---|
| Server process | active for weeks, `NRestarts=0`, bound to `:53/udp` and `:53/tcp` |
| Inbound packets from the test client network | **none** in either capture window |
| Unrelated internet scanner hitting `:53` | present, server answered `Refused` — proves reachability and a working service |
| `dig @203.0.113.130` (UDP, from client) | `NOERROR` — but absent from the server capture |
| `dig +tcp @203.0.113.130` | `NOERROR` — but absent from the server capture |
| `dig @192.0.2.1` / `@198.51.100.7` | **`NOERROR`** — impossible without interception |

Conclusion: the server was healthy the whole time; the client network transparently intercepts
outbound port 53 on both UDP and TCP, so tunnel packets never leave it.

## Why `nc -vz` is misleading here

`nc -vz host 53` reports success if **anything** completes the TCP handshake — including the
interceptor. It cannot distinguish "the server answered" from "the middlebox answered".
The same applies to `dig`: a `NOERROR` reply is not proof of delivery.

Trust only the server-side capture (or a tunnel-level end-to-end check such as
`curl -x socks5h://127.0.0.1:7000 https://api.ipify.org` returning the server's IP).

## Practical consequences

- A DNS tunnel client that sends UDP:53 to arbitrary public resolvers cannot work behind such
  a network; the packets are redirected or dropped.
- Retry storms in the client log (`retry 1/5 … 4/5`, `no downstream after N attempts`) are a
  typical symptom.
- Options: fix/disable the router's DNS interception, use a different network for the client,
  or use a transport that does not look like DNS:53 to arbitrary destinations.

## Reproducing

The capture files from the runs are kept locally with the operational workspace; the method
above is sufficient to repeat the diagnosis anywhere.
