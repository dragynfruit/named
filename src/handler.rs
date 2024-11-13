use crate::resolve::Resolver;
use dashmap::DashMap;
use hickory_proto::{op::Header, rr::Record};
use hickory_server::{
    authority::MessageResponseBuilder,
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
};
use log::{debug, error, info, warn};
use metrics::{counter, gauge};
use tokio::time::interval;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INITIAL_CACHE_SIZE: usize = 10_000;
const METRICS_INTERVAL: u64 = 60; // 1 minute
const CACHE_CLEANUP_INTERVAL: u64 = 60; // 1 minute

#[derive(Debug, Clone)]
struct CacheEntry {
    records: Vec<Record>,
    saved_at: u64,
    access_count: u64,
    last_access: u64,
    shortest_ttl: u32,
}

impl CacheEntry {
    fn new(records: Vec<Record>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let shortest_ttl = records
            .iter()
            .map(|record| record.ttl())
            .min()
            .unwrap_or(60);

        Self {
            records,
            saved_at: now,
            access_count: 1,
            last_access: now,
            shortest_ttl,
        }
    }

    fn access(&mut self) {
        self.access_count += 1;
        self.last_access = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();
    }

    fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        (now - self.saved_at) > self.shortest_ttl as u64
    }
}

#[derive(Debug)]
struct CacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    total_response_time: AtomicU64,
    response_count: AtomicU64,
}

impl CacheMetrics {
    fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            total_response_time: AtomicU64::new(0),
            response_count: AtomicU64::new(0),
        }
    }

    fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed) as f64;
        let misses = self.misses.load(Ordering::Relaxed) as f64;
        let total = hits + misses;

        if total == 0.0 {
            0.0
        } else {
            hits / total
        }
    }

    fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        counter!("dns_cache_hits").increment(1);
        debug!("Cache hit recorded");
    }

    fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        counter!("dns_cache_misses").increment(1);
        debug!("Cache miss recorded");
    }

    fn record_response_time(&self, duration_ms: u64) {
        self.total_response_time
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.response_count.fetch_add(1, Ordering::Relaxed);
        debug!("Response time recorded: {}ms", duration_ms);
    }

    fn avg_response_time(&self) -> f64 {
        let total = self.total_response_time.load(Ordering::Relaxed) as f64;
        let count = self.response_count.load(Ordering::Relaxed) as f64;
        if count > 0.0 {
            total / count
        } else {
            0.0
        }
    }

    fn reset(&self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.total_response_time.store(0, Ordering::Relaxed);
        self.response_count.store(0, Ordering::Relaxed);
        debug!("Metrics reset for new period");
    }
}

pub struct Handler {
    resolver: Resolver,
    cache: Arc<DashMap<String, CacheEntry>>,
    metrics: Arc<CacheMetrics>,
}

impl Handler {
    pub async fn new() -> Self {
        info!("Initializing DNS cache handler [cache_size={}, metrics_interval={}s, cleanup_interval={}s]",
            INITIAL_CACHE_SIZE, METRICS_INTERVAL, CACHE_CLEANUP_INTERVAL);

        let handler = Self {
            resolver: Resolver::new().await.map_err(|e| {
                error!("Failed to initialize resolver: {}", e);
                e
            }).unwrap(),
            cache: Arc::new(DashMap::with_capacity(INITIAL_CACHE_SIZE)),
            metrics: Arc::new(CacheMetrics::new()),
        };

        handler.start_cache_cleanup();
        handler.start_metrics_reporter();

        info!("DNS cache handler initialized successfully");
        handler
    }

    fn start_metrics_reporter(&self) {
        let metrics = self.metrics.clone();
        let cache = self.cache.clone();

        info!("Starting metrics reporter");

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(METRICS_INTERVAL));

            loop {
                interval.tick().await;

                let hit_rate = metrics.hit_rate();
                let cache_size = cache.len();
                let cache_capacity = cache.capacity();
                let avg_response = metrics.avg_response_time();
                let memory_usage = (cache_size * std::mem::size_of::<CacheEntry>()) / 1024 / 1024;

                // Report metrics
                gauge!("dns_cache_hit_rate").set(hit_rate as f64);
                gauge!("dns_cache_size").set(cache_size as f64);
                gauge!("dns_cache_capacity").set(cache_capacity as f64);
                gauge!("dns_cache_avg_response_time_ms").set(avg_response);

                info!(
                    "Cache metrics report: hit_rate={:.2}%, size={}/{}, memory={}MB, avg_response={:.2}ms",
                    hit_rate * 100.0,
                    cache_size,
                    cache_capacity,
                    memory_usage,
                    avg_response
                );

                metrics.reset();
            }
        });
    }

    fn start_cache_cleanup(&self) {
        let cache = self.cache.clone();

        info!("Starting cache cleanup task");

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(CACHE_CLEANUP_INTERVAL));

            loop {
                interval.tick().await;
                let initial_size = cache.len();
                cache.retain(|_, entry| !entry.is_expired());
                let cleaned_count = initial_size - cache.len();
                
                info!(
                    "Cache cleanup completed: removed={} entries, remaining={} entries",
                    cleaned_count,
                    cache.len()
                );
            }
        });
    }

    async fn resolve_or_cache(&self, request: &Request) -> Vec<Record> {
        let query = request.query().original();
        let query_name = query.name().to_string();
        let query_type = query.query_type().to_string();
        let key = format!("{query_name}/{query_type}");

        debug!(
            "Processing DNS query [client={}, name={}, type={}]",
            request.src(),
            query_name,
            query_type
        );

        // Check cache first
        if let Some(mut entry) = self.cache.get_mut(&key) {
            if !entry.is_expired() {
                entry.access();
                self.metrics.record_hit();
                let ttl_remaining = entry.shortest_ttl as i64 - 
                    (SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() - entry.saved_at) as i64;
                
                debug!(
                    "Cache hit [key={}, access_count={}, ttl_remaining={}s]",
                    key,
                    entry.access_count,
                    ttl_remaining
                );
                return entry.records.clone();
            }
        }

        self.metrics.record_miss();
        debug!("Cache miss [key={}]", key);

        // Try to resolve
        match self.resolver.resolve(request).await {
            Ok(records) => {
                let entry = CacheEntry::new(records.clone());
                debug!(
                    "Resolution successful [key={}, records={}, ttl={}s]",
                    key,
                    records.len(),
                    entry.shortest_ttl
                );
                self.cache.insert(key, entry);
                records
            }
            Err(e) => {
                warn!(
                    "Resolver error, attempting to use expired cached data: {}",
                    e
                );

                // On failure, try to use expired cache entry if available
                if let Some(entry) = self.cache.get(&key) {
                    let entry_age = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() - entry.saved_at;
                    
                    warn!(
                        "Using expired cache entry [key={}, age={}s]",
                        key,
                        entry_age
                    );
                    entry.records.clone()
                } else {
                    error!(
                        "No cached data available and resolver failed [key={}]",
                        key
                    );
                    vec![]
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl RequestHandler for Handler {
    async fn handle_request<R>(&self, request: &Request, mut response_handle: R) -> ResponseInfo
    where
        R: ResponseHandler,
    {
        let start = SystemTime::now();
        let query_name = request.query().original().name();

        debug!(
            "Handling DNS request [client={}, query={}]",
            request.src(),
            query_name
        );

        let builder = MessageResponseBuilder::from_message_request(request);
        let header = Header::response_from_request(request.header());
        let records = self.resolve_or_cache(request).await;

        let response_info = if records.is_empty() {
            debug!("No records found, sending empty response [query={}]", query_name);
            let response = builder.build_no_records(header);
            response_handle.send_response(response).await
        } else {
            debug!(
                "Sending response [query={}, records={}]",
                query_name,
                records.len()
            );
            let response = builder.build(header, records.iter(), vec![], vec![], vec![]);
            response_handle.send_response(response).await
        };

        let result = match response_info {
            Ok(result) => result,
            Err(e) => {
                warn!(
                    "Error sending response to client [query={}, error={}]",
                    query_name,
                    e
                );
                ResponseInfo::from(header)
            }
        };

        let elapsed = SystemTime::now()
            .duration_since(start)
            .expect("Time went backwards")
            .as_millis();

        debug!(
            "Request completed [client={}, query={}, duration={}ms, response_code={:?}, records={}]",
            request.src(),
            query_name,
            elapsed,
            result.response_code(),
            records.len()
        );

        self.metrics.record_response_time(elapsed as u64);
        result
    }
}