/// A bundled known public DNS resolver.
#[derive(Debug, Clone)]
pub struct BundledResolver {
    pub name: &'static str,
    pub ip: &'static str,
    pub doh_url: Option<&'static str>,
}

/// Returns the bundled list of known public DNS resolvers.
pub fn bundled_resolvers() -> Vec<BundledResolver> {
    vec![
        // === Cloudflare ===
        BundledResolver { name: "Cloudflare", ip: "1.1.1.1", doh_url: Some("https://cloudflare-dns.com/dns-query") },
        BundledResolver { name: "Cloudflare", ip: "1.0.0.1", doh_url: Some("https://cloudflare-dns.com/dns-query") },
        BundledResolver { name: "Cloudflare-Security", ip: "1.1.1.2", doh_url: Some("https://security.cloudflare-dns.com/dns-query") },
        BundledResolver { name: "Cloudflare-Family", ip: "1.1.1.3", doh_url: Some("https://family.cloudflare-dns.com/dns-query") },
        // === Google ===
        BundledResolver { name: "Google", ip: "8.8.8.8", doh_url: Some("https://dns.google/dns-query") },
        BundledResolver { name: "Google", ip: "8.8.4.4", doh_url: Some("https://dns.google/dns-query") },
        // === Quad9 ===
        BundledResolver { name: "Quad9", ip: "9.9.9.9", doh_url: Some("https://dns.quad9.net/dns-query") },
        BundledResolver { name: "Quad9", ip: "149.112.112.112", doh_url: Some("https://dns.quad9.net/dns-query") },
        BundledResolver { name: "Quad9-Unsecured", ip: "9.9.9.10", doh_url: Some("https://dns10.quad9.net/dns-query") },
        // === OpenDNS ===
        BundledResolver { name: "OpenDNS", ip: "208.67.222.222", doh_url: Some("https://doh.opendns.com/dns-query") },
        BundledResolver { name: "OpenDNS", ip: "208.67.220.220", doh_url: Some("https://doh.opendns.com/dns-query") },
        BundledResolver { name: "OpenDNS-Family", ip: "208.67.222.123", doh_url: Some("https://doh.familyshield.opendns.com/dns-query") },
        // === AdGuard ===
        BundledResolver { name: "AdGuard", ip: "94.140.14.14", doh_url: Some("https://dns.adguard-dns.com/dns-query") },
        BundledResolver { name: "AdGuard", ip: "94.140.15.15", doh_url: Some("https://dns.adguard-dns.com/dns-query") },
        BundledResolver { name: "AdGuard-Family", ip: "94.140.14.15", doh_url: Some("https://dns-family.adguard-dns.com/dns-query") },
        BundledResolver { name: "AdGuard-Unfiltered", ip: "94.140.14.140", doh_url: Some("https://unfiltered.adguard-dns.com/dns-query") },
        // === NextDNS ===
        BundledResolver { name: "NextDNS", ip: "45.90.28.0", doh_url: Some("https://dns.nextdns.io/") },
        BundledResolver { name: "NextDNS", ip: "45.90.30.0", doh_url: Some("https://dns.nextdns.io/") },
        // === CleanBrowsing ===
        BundledResolver { name: "CleanBrowsing-Security", ip: "185.228.168.9", doh_url: Some("https://doh.cleanbrowsing.org/doh/security-filter") },
        BundledResolver { name: "CleanBrowsing-Family", ip: "185.228.168.168", doh_url: Some("https://doh.cleanbrowsing.org/doh/family-filter") },
        BundledResolver { name: "CleanBrowsing-Adult", ip: "185.228.168.10", doh_url: Some("https://doh.cleanbrowsing.org/doh/adult-filter") },
        // === Mullvad ===
        BundledResolver { name: "Mullvad", ip: "194.242.2.2", doh_url: Some("https://dns.mullvad.net/dns-query") },
        BundledResolver { name: "Mullvad-AdBlock", ip: "194.242.2.3", doh_url: Some("https://adblock.dns.mullvad.net/dns-query") },
        // === DNS.SB ===
        BundledResolver { name: "DNS.SB", ip: "185.222.222.222", doh_url: Some("https://doh.dns.sb/dns-query") },
        BundledResolver { name: "DNS.SB", ip: "45.11.45.11", doh_url: Some("https://doh.dns.sb/dns-query") },
        // === AliDNS ===
        BundledResolver { name: "AliDNS", ip: "223.5.5.5", doh_url: Some("https://dns.alidns.com/dns-query") },
        BundledResolver { name: "AliDNS", ip: "223.6.6.6", doh_url: Some("https://dns.alidns.com/dns-query") },
        // === DNSPod (Tencent) ===
        BundledResolver { name: "DNSPod", ip: "119.29.29.29", doh_url: Some("https://doh.pub/dns-query") },
        // === Comodo Secure ===
        BundledResolver { name: "Comodo", ip: "8.26.56.26", doh_url: None },
        BundledResolver { name: "Comodo", ip: "8.20.247.20", doh_url: None },
        // === Neustar/Vercara ===
        BundledResolver { name: "Neustar", ip: "64.6.64.6", doh_url: None },
        BundledResolver { name: "Neustar", ip: "64.6.65.6", doh_url: None },
        // === Level3/CenturyLink ===
        BundledResolver { name: "Level3", ip: "4.2.2.1", doh_url: None },
        BundledResolver { name: "Level3", ip: "4.2.2.2", doh_url: None },
        BundledResolver { name: "Level3", ip: "4.2.2.3", doh_url: None },
        BundledResolver { name: "Level3", ip: "4.2.2.4", doh_url: None },
        // === Yandex ===
        BundledResolver { name: "Yandex", ip: "77.88.8.8", doh_url: None },
        BundledResolver { name: "Yandex", ip: "77.88.8.1", doh_url: None },
        // === Freenom ===
        BundledResolver { name: "Freenom", ip: "80.80.80.80", doh_url: None },
        BundledResolver { name: "Freenom", ip: "80.80.81.81", doh_url: None },
        // === UncensoredDNS ===
        BundledResolver { name: "UncensoredDNS", ip: "91.239.100.100", doh_url: None },
        // === Hurricane Electric ===
        BundledResolver { name: "HurricaneElectric", ip: "74.82.42.42", doh_url: None },
        // === Verisign ===
        BundledResolver { name: "Verisign", ip: "64.6.64.6", doh_url: None },
        BundledResolver { name: "Verisign", ip: "64.6.65.6", doh_url: None },
        // === SafeDNS ===
        BundledResolver { name: "SafeDNS", ip: "195.46.39.39", doh_url: None },
        BundledResolver { name: "SafeDNS", ip: "195.46.39.40", doh_url: None },
        // === OpenNIC ===
        BundledResolver { name: "OpenNIC", ip: "94.247.43.254", doh_url: None },
        // === Dyn ===
        BundledResolver { name: "Dyn", ip: "216.146.35.35", doh_url: None },
        BundledResolver { name: "Dyn", ip: "216.146.36.36", doh_url: None },
        // === Control D ===
        BundledResolver { name: "ControlD", ip: "76.76.2.0", doh_url: Some("https://freedns.controld.com/p0") },
        BundledResolver { name: "ControlD", ip: "76.76.10.0", doh_url: Some("https://freedns.controld.com/p0") },
        // === LibreDNS ===
        BundledResolver { name: "LibreDNS", ip: "116.202.176.26", doh_url: Some("https://doh.libredns.gr/dns-query") },
    ]
}
