use anyhow::Result;
use sp_common::config::{Config, ResolverConfig};
use std::path::Path;

const CACHE_FILE: &str = "resolver-cache.toml";

/// Load cached resolvers from previous run.
/// Returns None if cache doesn't exist or is corrupted.
pub fn load_cache(cache_dir: &Path) -> Option<Config> {
    let path = cache_dir.join(CACHE_FILE);
    if !path.exists() {
        return None;
    }
    Config::load(&path).ok()
}

/// Save working resolvers to cache file.
pub fn save_cache(cache_dir: &Path, resolvers: &[ResolverConfig]) -> Result<()> {
    let path = cache_dir.join(CACHE_FILE);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut content = String::from("# Auto-discovered resolver cache\n");
    content.push_str(&format!("# Updated: {}\n\n", ts));

    for r in resolvers {
        content.push_str("[[resolver]]\n");
        content.push_str(&format!("name = \"{}\"\n", r.name));
        let ips: Vec<String> = r.ip.iter().map(|ip| format!("\"{}\"", ip)).collect();
        content.push_str(&format!("ip = [{}]\n", ips.join(", ")));
        if let Some(ref doh) = r.doh {
            content.push_str(&format!("doh = \"{}\"\n", doh));
        }
        if r.ech {
            content.push_str("ech = true\n");
        }
        if let Some(edns0) = r.edns0_max {
            content.push_str(&format!("edns0_max = {}\n", edns0));
        }
        if let Some(rps) = r.rate_limit_rps {
            content.push_str(&format!("rate_limit_rps = {}\n", rps));
        }
        content.push('\n');
    }

    std::fs::create_dir_all(cache_dir)?;
    std::fs::write(&path, content)?;
    Ok(())
}
