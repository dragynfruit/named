use serde::Deserialize;
use std::error::Error;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub dns_addr: String,
    pub dns_timeout: u64,
    pub dns_host: String,
    pub host: String,
    pub port: u16,
    pub initial_cache_size: usize,
    pub metrics_interval: u64,
    pub cache_cleanup_interval: u64,
}

impl Config {
    pub fn new() -> Self {
        Self {
            dns_addr: "1.1.1.1:443".to_string(),
            dns_timeout: 5,
            dns_host: "cloudflare-dns.com".to_string(),
            host: "127.0.0.2".to_string(),
            port: 53,
            initial_cache_size: 10_000,
            metrics_interval: 60,
            cache_cleanup_interval: 60,
        }
    }

    pub fn from_file(path: &str) -> Result<Self, Box<dyn Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
