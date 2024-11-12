use std::env;

use crate::handler::Handler;
use hickory_server::ServerFuture;
use tokio::net::UdpSocket;

mod handler;
mod resolve;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let addr = env::var("ADDR").unwrap_or_else(|_| "127.0.0.2".to_string());
    let port = env::var("PORT").unwrap_or_else(|_| "53".to_string());
    let handler = Handler::new().await;

    let mut server = ServerFuture::new(handler);
    let udp = UdpSocket::bind(format!("{}:{}", addr, port))
        .await
        .expect("Failed to bind UDP socket");
    server.register_socket(udp);
    log::info!("Listening at {}:{}", addr, port);

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");
    server
        .shutdown_gracefully()
        .await
        .expect("Failed to gracefully shutdown server");

    log::info!("Server shutdown");
}
