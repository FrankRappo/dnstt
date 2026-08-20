use std::net::Ipv4Addr;

/// Check if an IPv4 address is in a reserved/private range and should be excluded from scanning.
pub fn is_excluded(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    matches!(octets,
        // 0.0.0.0/8 — "This" network
        [0, ..] |
        // 10.0.0.0/8 — Private
        [10, ..] |
        // 127.0.0.0/8 — Loopback
        [127, ..] |
        // 169.254.0.0/16 — Link-local
        [169, 254, ..] |
        // 192.0.0.0/24 — IETF protocol assignments
        [192, 0, 0, _] |
        // 192.0.2.0/24 — TEST-NET-1
        [192, 0, 2, _] |
        // 192.168.0.0/16 — Private
        [192, 168, ..] |
        // 198.51.100.0/24 — TEST-NET-2
        [198, 51, 100, _] |
        // 203.0.113.0/24 — TEST-NET-3
        [203, 0, 113, _] |
        // 255.255.255.255/32 — Broadcast
        [255, 255, 255, 255]
    )
    || is_100_64(octets)    // 100.64.0.0/10 — Shared address space (CGNAT)
    || is_172_16(octets)    // 172.16.0.0/12 — Private
    || is_198_18(octets)    // 198.18.0.0/15 — Benchmarking
    || is_multicast(octets) // 224.0.0.0/4 — Multicast
    || is_reserved(octets)  // 240.0.0.0/4 — Reserved
}

// 100.64.0.0/10: first two bits of second octet must be 01
fn is_100_64(o: [u8; 4]) -> bool {
    o[0] == 100 && (o[1] & 0xC0) == 64
}

// 172.16.0.0/12: second octet 16..31
fn is_172_16(o: [u8; 4]) -> bool {
    o[0] == 172 && (o[1] & 0xF0) == 16
}

// 198.18.0.0/15: second octet 18 or 19
fn is_198_18(o: [u8; 4]) -> bool {
    o[0] == 198 && (o[1] == 18 || o[1] == 19)
}

// 224.0.0.0/4
fn is_multicast(o: [u8; 4]) -> bool {
    (o[0] & 0xF0) == 224
}

// 240.0.0.0/4
fn is_reserved(o: [u8; 4]) -> bool {
    (o[0] & 0xF0) == 240
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_excluded() {
        assert!(is_excluded("0.0.0.0".parse().unwrap()));
        assert!(is_excluded("10.0.0.1".parse().unwrap()));
        assert!(is_excluded("100.64.0.1".parse().unwrap()));
        assert!(is_excluded("100.127.255.255".parse().unwrap()));
        assert!(!is_excluded("100.128.0.0".parse().unwrap()));
        assert!(is_excluded("127.0.0.1".parse().unwrap()));
        assert!(is_excluded("169.254.1.1".parse().unwrap()));
        assert!(is_excluded("172.16.0.1".parse().unwrap()));
        assert!(is_excluded("172.31.255.255".parse().unwrap()));
        assert!(!is_excluded("172.32.0.0".parse().unwrap()));
        assert!(is_excluded("192.0.0.1".parse().unwrap()));
        assert!(is_excluded("192.0.2.1".parse().unwrap()));
        assert!(is_excluded("192.168.0.1".parse().unwrap()));
        assert!(is_excluded("198.18.0.1".parse().unwrap()));
        assert!(is_excluded("198.19.255.255".parse().unwrap()));
        assert!(is_excluded("198.51.100.1".parse().unwrap()));
        assert!(is_excluded("203.0.113.1".parse().unwrap()));
        assert!(is_excluded("224.0.0.1".parse().unwrap()));
        assert!(is_excluded("240.0.0.1".parse().unwrap()));
        assert!(is_excluded("255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn test_not_excluded() {
        assert!(!is_excluded("1.1.1.1".parse().unwrap()));
        assert!(!is_excluded("8.8.8.8".parse().unwrap()));
        assert!(!is_excluded("9.9.9.9".parse().unwrap()));
        assert!(!is_excluded("208.67.222.222".parse().unwrap()));
    }
}
