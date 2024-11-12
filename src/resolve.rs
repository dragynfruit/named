use hickory_client::{client::{AsyncClient, ClientHandle}, error::{ClientError, ClientErrorKind}};
use hickory_proto::{
    error::ProtoError,
    h2::{HttpsClientConnect, HttpsClientStreamBuilder},
    iocompat::AsyncIoTokioAsStd,
    rr::Record,
};
use hickory_server::server::Request;
use once_cell::sync::Lazy;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::RwLock,
    time::timeout,
};

// Constants for configuration
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443);
const DNS_HOST: &str = "cloudflare-dns.com";
// const DNS_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(80, 152, 203, 134)), 443);
// const DNS_HOST: &str = "doh.kekew.info";

static CONFIG: Lazy<rustls::ClientConfig> = Lazy::new(|| rustls_platform_verifier::tls_config());

#[derive(Error, Debug)]
pub enum ResolverError {
    #[error("Protocol error: {0}")]
    Proto(#[from] ProtoError),
    #[error("Client error: {0}")]
    Client(#[from] ClientError),
    #[error("Request timeout")]
    Timeout,
}

pub struct Resolver {
    client: RwLock<AsyncClient>,
}

impl Resolver {
    async fn create_client() -> Result<AsyncClient, ResolverError> {
        let client_config = Arc::new(CONFIG.clone());
        let connection: HttpsClientConnect<AsyncIoTokioAsStd<TcpStream>> = HttpsClientStreamBuilder::with_client_config(client_config)
            .build(DNS_ADDR, DNS_HOST.to_string());

        let (client, task) = AsyncClient::connect(connection).await?;
        tokio::spawn(task);
        Ok(client)
    }

    pub async fn new() -> Result<Self, ResolverError> {
        log::info!("Creating DNS client");
        let client = Self::create_client().await?;

        Ok(Self {
            client: RwLock::new(client),
        })
    }

    pub async fn resolve(&self, request: &Request) -> Result<Vec<Record>, ResolverError> { // need to handle being busy, it could mean multiple people are using the client
        let mut client = self.client.read().await.clone();
        
        let query_result = timeout(
            DNS_TIMEOUT,
            client.query(
                request.query().original().name().clone(),
                request.query().query_class(),
                request.query().query_type(),
            ),
        ).await;

        match query_result {
            Ok(Ok(response)) => {
                Ok(response.answers().to_vec())
            },
            Ok(Err(e)) => {
                log::warn!("DNS query error, creating new client: {:?}", e);

                match e.kind() {
                    ClientErrorKind::Proto(err) => {
                        if err.is_busy() {
                            *self.client.write().await = Self::create_client().await?;
                        }
                    },
                    _ => {}
                }

                Err(ResolverError::Client(e))
            },
            Err(_) => {
                // Timeout occurred
                Err(ResolverError::Timeout)
            }
        }
    }
}