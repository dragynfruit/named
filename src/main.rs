use std::env;

use crate::handler::Handler;
use hickory_server::ServerFuture;
use tokio::net::UdpSocket;

mod handler;
mod resolve;
mod config;

const HELP: &str = "\
Named

USAGE:
  named [OPTIONS]

FLAGS:
    -h, --help                        Prints help information

OPTIONS:
    --host <host>                     Host to listen on [default: 127.0.0.2]
    --port <port>                     Port to listen on [default: 53]
    --config <config>                 Path to configuration file
    --dns-addr <dns_addr>             DNS server address [default: 1.1.1.1:443]
    --dns-timeout <dns_timeout>       DNS query timeout in seconds [default: 5]
    --dns-host <dns_host>             DNS server host [default: cloudflare-dns.com]
    --initial-cache-size <size>       Initial cache size [default: 10000]
    --metrics-interval <secs>         Metrics reporting interval in seconds [default: 60]
    --cache-cleanup-interval <secs>   Cache cleanup interval in seconds [default: 60]
";

struct AppArgs {
    // Serve options
    host: Option<String>,
    port: Option<String>,
    config_path: Option<String>,
    
    // Resolve options
    dns_addr: Option<String>,
    dns_timeout: Option<String>,
    dns_host: Option<String>,
    
    // Handle options
    initial_cache_size: Option<String>,
    metrics_interval: Option<String>,
    cache_cleanup_interval: Option<String>,
}

fn parse_args() -> Result<AppArgs, pico_args::Error> {
    let mut pargs = pico_args::Arguments::from_env();

    // Help has a higher priority and should be handled separately.
    if pargs.contains(["-h", "--help"]) {
        print!("{}", HELP);
        std::process::exit(0);
    }

    let args = AppArgs {
        host: pargs.opt_value_from_str("--host")?,
        port: pargs.opt_value_from_str("--port")?,
        config_path: pargs.opt_value_from_str("--config")?,
        dns_addr: pargs.opt_value_from_str("--dns-addr")?,
        dns_timeout: pargs.opt_value_from_str("--dns-timeout")?,
        dns_host: pargs.opt_value_from_str("--dns-host")?,
        initial_cache_size: pargs.opt_value_from_str("--initial-cache-size")?,
        metrics_interval: pargs.opt_value_from_str("--metrics-interval")?,
        cache_cleanup_interval: pargs.opt_value_from_str("--cache-cleanup-interval")?,
    };

    // It's up to the caller what to do with the remaining arguments.
    let remaining = pargs.finish();
    if !remaining.is_empty() {
        eprintln!("Warning: unused arguments left: {:?}.", remaining);
    }

    Ok(args)
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: {}.", e);
            std::process::exit(1);
        }
    };

    // Load configurations
    let mut config = config::Config::new();

    if let Some(ref config_path) = args.config_path {
        config = config::Config::from_file(config_path)
            .expect("Failed to load config file");
    } else if let Ok(config_path) = env::var("CONFIG_PATH") {
        config = config::Config::from_file(&config_path)
            .expect("Failed to load config file");
    }

    // Override with command line arguments or environment variables
    config.serve.host = args.host
        .or_else(|| env::var("HOST").ok())
        .unwrap_or(config.serve.host);

    config.serve.port = args.port
        .or_else(|| env::var("PORT").ok())
        .and_then(|p| p.parse().ok())
        .unwrap_or(config.serve.port);

    config.resolve.dns_addr = args.dns_addr
        .or_else(|| env::var("DNS_ADDR").ok())
        .unwrap_or(config.resolve.dns_addr);

    config.resolve.dns_timeout = args.dns_timeout
        .or_else(|| env::var("DNS_TIMEOUT").ok())
        .and_then(|t| t.parse().ok())
        .unwrap_or(config.resolve.dns_timeout);

    config.resolve.dns_host = args.dns_host
        .or_else(|| env::var("DNS_HOST").ok())
        .unwrap_or(config.resolve.dns_host);

    config.handle.initial_cache_size = args.initial_cache_size
        .or_else(|| env::var("INITIAL_CACHE_SIZE").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(config.handle.initial_cache_size);

    config.handle.metrics_interval = args.metrics_interval
        .or_else(|| env::var("METRICS_INTERVAL").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(config.handle.metrics_interval);

    config.handle.cache_cleanup_interval = args.cache_cleanup_interval
        .or_else(|| env::var("CACHE_CLEANUP_INTERVAL").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(config.handle.cache_cleanup_interval);

    let handler = Handler::new(&config).await;

    let mut server = ServerFuture::new(handler);
    let udp = UdpSocket::bind(format!("{}:{}", config.serve.host, config.serve.port))
        .await
        .expect("Failed to bind UDP socket");
    server.register_socket(udp);
    log::info!("Listening at {}:{}", config.serve.host, config.serve.port);

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");
    server
        .shutdown_gracefully()
        .await
        .expect("Failed to gracefully shutdown server");

    log::info!("Server shutdown");
}
