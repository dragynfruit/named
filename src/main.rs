use std::env;

use crate::handler::Handler;
use hickory_server::ServerFuture;
use tokio::net::UdpSocket;

mod handler;
mod resolve;

const HELP: &str = "\
Named

USAGE:
  named [OPTIONS]

FLAGS:
    -h, --help            Prints help information

OPTIONS:
    --host <host>         Host to listen on [default: 127.0.0.2]
    --port <port>         Port to listen on [default: 53]
";

struct AppArgs {
    host: Option<String>,
    port: Option<String>,
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

    let host = args.host.unwrap_or_else(|| env::var("ADDR").unwrap_or_else(|_| "127.0.0.2".to_string()));
    let port = args.port.unwrap_or_else(|| env::var("PORT").unwrap_or_else(|_| "53".to_string()));
    let handler = Handler::new().await;

    let mut server = ServerFuture::new(handler);
    let udp = UdpSocket::bind(format!("{}:{}", host, port))
        .await
        .expect("Failed to bind UDP socket");
    server.register_socket(udp);
    log::info!("Listening at {}:{}", host, port);

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");
    server
        .shutdown_gracefully()
        .await
        .expect("Failed to gracefully shutdown server");

    log::info!("Server shutdown");
}
