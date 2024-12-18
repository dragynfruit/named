use hickory_client::{client::{AsyncClient, ClientHandle}, error::{ClientError, ClientErrorKind}};
use hickory_proto::{
    error::ProtoError,
    h2::{HttpsClientConnect, HttpsClientStreamBuilder},
    iocompat::AsyncIoTokioAsStd,
    rr::Record,
};
use hickory_server::server::Request;
use log::{debug, error, info, warn};
use metrics::{counter, gauge, histogram};
use once_cell::sync::Lazy;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::RwLock,
    time::{timeout, Instant},
};
use rustls::{ClientConfig, OwnedTrustAnchor, RootCertStore};
use webpki_roots::TLS_SERVER_ROOTS;
use crate::config::Config;

// Constants for configuration
static CONFIG: Lazy<ClientConfig> = Lazy::new(|| {
    info!("Initializing TLS configuration");
    let mut root_store = RootCertStore::empty();
    root_store.add_trust_anchors(TLS_SERVER_ROOTS.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            ta.subject.as_ref().to_vec(),
            ta.subject_public_key_info.as_ref().to_vec(),
            ta.name_constraints.as_ref().map(|nc| nc.as_ref().to_vec()),
        )
    }));
    ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth()
});

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
    client_creation_count: std::sync::atomic::AtomicU64,
    last_client_creation: std::sync::atomic::AtomicU64,
    dns_addr: SocketAddr,
    dns_host: String,
    dns_timeout: Duration,
}

impl Resolver {
    async fn create_client(dns_addr: &SocketAddr, dns_host: &str) -> Result<AsyncClient, ResolverError> {
        let start = Instant::now();
        info!(
            "Creating new DNS client [endpoint={}, host={}]",
            dns_addr,
            dns_host
        );

        let client_config = Arc::new(CONFIG.clone());
        
        debug!("Establishing HTTPS connection to DNS server");
        let connection: HttpsClientConnect<AsyncIoTokioAsStd<TcpStream>> = HttpsClientStreamBuilder::with_client_config(client_config)
            .build(*dns_addr, dns_host.to_string());

        match AsyncClient::connect(connection).await {
            Ok((client, task)) => {
                let elapsed = start.elapsed();
                info!(
                    "DNS client created successfully [duration={}ms]",
                    elapsed.as_millis()
                );
                histogram!("dns_client_creation_time").record(elapsed);
                
                tokio::spawn(task);
                Ok(client)
            }
            Err(e) => {
                error!(
                    "Failed to create DNS client: {} [duration={}ms]",
                    e,
                    start.elapsed().as_millis()
                );
                counter!("dns_client_creation_failures").increment(1);
                Err(ResolverError::Client(ClientError::from(ClientErrorKind::Proto(e))))
            }
        }
    }

    pub async fn new(config: &Config) -> Result<Self, ResolverError> {
        info!("Initializing DNS resolver");
        let start = Instant::now();

        let dns_addr: SocketAddr = config.dns_addr.parse().expect("Invalid DNS address");
        let dns_host = config.dns_host.clone();
        let dns_timeout = Duration::from_secs(config.dns_timeout);

        let client = Self::create_client(&dns_addr, &dns_host).await?;
        let resolver = Self {
            client: RwLock::new(client),
            client_creation_count: std::sync::atomic::AtomicU64::new(1),
            last_client_creation: std::sync::atomic::AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            ),
            dns_addr,
            dns_host,
            dns_timeout,
        };

        info!(
            "DNS resolver initialized successfully [duration={}ms]",
            start.elapsed().as_millis()
        );
        Ok(resolver)
    }

    pub async fn resolve(&self, request: &Request) -> Result<Vec<Record>, ResolverError> {
        let start = Instant::now();
        let query = request.query().original();
        let query_name = query.name();
        let query_type = query.query_type();
        
        debug!(
            "Processing DNS query [name={}, type={}, class={}]",
            query_name,
            query_type,
            request.query().query_class()
        );

        let mut client = self.client.read().await.clone();
        
        let query_result = timeout(
            self.dns_timeout,
            client.query(
                query_name.clone(),
                request.query().query_class(),
                query_type,
            ),
        ).await;

        match query_result {
            Ok(Ok(response)) => {
                let elapsed = start.elapsed();
                let answer_count = response.answers().len();
                
                debug!(
                    "DNS query successful [name={}, type={}, answers={}, duration={}ms]",
                    query_name,
                    query_type,
                    answer_count,
                    elapsed.as_millis()
                );

                histogram!("dns_query_time").record(elapsed);
                counter!("dns_query_success").increment(1);
                gauge!("dns_response_size").set(answer_count as f64);

                Ok(response.answers().to_vec())
            },
            Ok(Err(e)) => {
                warn!(
                    "DNS query error [name={}, type={}, error={:?}]",
                    query_name,
                    query_type,
                    e
                );

                counter!("dns_query_errors").increment(1);

                match e.kind() {
                    ClientErrorKind::Proto(err) => {
                        if err.is_busy() {
                            warn!(
                                "DNS client busy, creating new client [last_creation_age={}s]",
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs()
                                    - self.last_client_creation.load(std::sync::atomic::Ordering::Relaxed)
                            );

                            *self.client.write().await = Self::create_client(&self.dns_addr, &self.dns_host).await?;
                            
                            self.client_creation_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            self.last_client_creation.store(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                                std::sync::atomic::Ordering::Relaxed,
                            );

                            counter!("dns_client_busy_recreations").increment(1);
                        }
                    },
                    _ => {}
                }
                Err(ResolverError::Client(e))
            },
            Err(_) => {
                warn!(
                    "DNS query timeout [name={}, type={}, timeout={}s]",
                    query_name,
                    query_type,
                    self.dns_timeout.as_secs()
                );
                
                counter!("dns_query_timeouts").increment(1);
                Err(ResolverError::Timeout)
            }
        }
    }
}