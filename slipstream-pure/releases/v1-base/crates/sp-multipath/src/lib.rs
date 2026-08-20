pub mod metrics;
pub mod pacing;

use anyhow::Result;
use bytes::Bytes;
use dashmap::DashMap;
use sp_common::config::ResolverEndpoint;
use sp_transport::{DnsQuery, DnsResponse, Transport};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use metrics::PathMetrics;
use pacing::AdaptivePacer;

/// A virtual path through one resolver with one transport.
pub struct VirtualPath {
    pub id: PathId,
    pub endpoint: ResolverEndpoint,
    pub transport: Arc<dyn Transport>,
    pub metrics: PathMetrics,
    pub pacer: AdaptivePacer,
    pub active: bool,
}

/// Unique path identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathId(pub u64);

impl std::fmt::Display for PathId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "path-{}", self.0)
    }
}

/// Multipath router: distributes DNS queries across all available resolver paths.
pub struct MultipathRouter {
    paths: Arc<RwLock<Vec<VirtualPath>>>,
    next_path_id: std::sync::atomic::AtomicU64,
    /// Round-robin index
    rr_index: std::sync::atomic::AtomicUsize,
}

impl MultipathRouter {
    pub fn new() -> Self {
        Self {
            paths: Arc::new(RwLock::new(Vec::new())),
            next_path_id: std::sync::atomic::AtomicU64::new(0),
            rr_index: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Add a new path (resolver + transport).
    pub async fn add_path(
        &self,
        endpoint: ResolverEndpoint,
        transport: Arc<dyn Transport>,
    ) -> PathId {
        let id = PathId(
            self.next_path_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let path = VirtualPath {
            id,
            endpoint,
            transport,
            metrics: PathMetrics::new(),
            pacer: AdaptivePacer::new(),
            active: true,
        };
        self.paths.write().await.push(path);
        debug!("added path {}", id);
        id
    }

    /// Remove a path by ID.
    pub async fn remove_path(&self, id: PathId) {
        let mut paths = self.paths.write().await;
        paths.retain(|p| p.id != id);
        debug!("removed path {}", id);
    }

    /// Get count of active paths.
    pub async fn active_path_count(&self) -> usize {
        self.paths.read().await.iter().filter(|p| p.active).count()
    }

    /// Send a DNS query through the best available path.
    ///
    /// Strategy: weighted round-robin based on path quality metrics.
    /// Falls back to random if all paths have equal weight.
    pub async fn send(&self, query: &DnsQuery) -> Result<(DnsResponse, PathId)> {
        let paths = self.paths.read().await;
        let active: Vec<usize> = paths
            .iter()
            .enumerate()
            .filter(|(_, p)| p.active && p.pacer.can_send())
            .map(|(i, _)| i)
            .collect();

        if active.is_empty() {
            // All paths paused — find the one with shortest backoff remaining
            drop(paths);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            return Err(anyhow::anyhow!("all paths are rate-limited or inactive"));
        }

        // Weighted round-robin: pick next active path
        let idx = self
            .rr_index
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % active.len();
        let path_idx = active[idx];
        let path_id = paths[path_idx].id;
        let transport = paths[path_idx].transport.clone();
        drop(paths);

        let start = std::time::Instant::now();
        let result = transport.exchange(query).await;
        let elapsed = start.elapsed();

        // Update metrics
        let mut paths = self.paths.write().await;
        if let Some(path) = paths.iter_mut().find(|p| p.id == path_id) {
            match &result {
                Ok(_) => {
                    path.metrics.record_success(elapsed);
                    path.pacer.on_success();
                }
                Err(e) => {
                    path.metrics.record_failure();
                    path.pacer.on_failure();
                    if path.metrics.failure_rate() > 0.5 {
                        warn!(
                            "path {} ({}) failure rate {:.0}%, deactivating",
                            path.id,
                            path.endpoint.name,
                            path.metrics.failure_rate() * 100.0
                        );
                        path.active = false;
                    }
                }
            }
        }

        result.map(|r| (r, path_id))
    }

    /// Send through a specific path (for retries).
    pub async fn send_via(&self, query: &DnsQuery, path_id: PathId) -> Result<DnsResponse> {
        let paths = self.paths.read().await;
        let path = paths
            .iter()
            .find(|p| p.id == path_id)
            .ok_or_else(|| anyhow::anyhow!("path {} not found", path_id))?;
        let transport = path.transport.clone();
        drop(paths);

        let start = std::time::Instant::now();
        let result = transport.exchange(query).await;
        let elapsed = start.elapsed();

        let mut paths = self.paths.write().await;
        if let Some(path) = paths.iter_mut().find(|p| p.id == path_id) {
            match &result {
                Ok(_) => {
                    path.metrics.record_success(elapsed);
                    path.pacer.on_success();
                }
                Err(_) => {
                    path.metrics.record_failure();
                    path.pacer.on_failure();
                }
            }
        }

        result
    }

    /// Broadcast a query to all active paths, return first success.
    pub async fn broadcast(&self, query: &DnsQuery) -> Result<DnsResponse> {
        let paths = self.paths.read().await;
        let transports: Vec<(PathId, Arc<dyn Transport>)> = paths
            .iter()
            .filter(|p| p.active)
            .map(|p| (p.id, p.transport.clone()))
            .collect();
        drop(paths);

        if transports.is_empty() {
            anyhow::bail!("no active paths for broadcast");
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(transports.len());
        for (pid, transport) in transports {
            let q = query.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Ok(resp) = transport.exchange(&q).await {
                    let _ = tx.send((resp, pid)).await;
                }
            });
        }
        drop(tx);

        rx.recv()
            .await
            .map(|(resp, _)| resp)
            .ok_or_else(|| anyhow::anyhow!("all broadcast paths failed"))
    }

    /// Get a snapshot of all path metrics.
    pub async fn path_stats(&self) -> Vec<PathStat> {
        self.paths
            .read()
            .await
            .iter()
            .map(|p| PathStat {
                id: p.id,
                name: p.endpoint.name.clone(),
                ip: p.endpoint.ip.clone(),
                transport: p.transport.transport_type(),
                active: p.active,
                avg_latency_ms: p.metrics.avg_latency().as_millis() as u64,
                success_count: p.metrics.success_count,
                failure_count: p.metrics.failure_count,
                failure_rate: p.metrics.failure_rate(),
            })
            .collect()
    }

    /// Reactivate all paths (for periodic recovery).
    pub async fn reactivate_all(&self) {
        let mut paths = self.paths.write().await;
        for p in paths.iter_mut() {
            if !p.active {
                debug!("reactivating path {} ({})", p.id, p.endpoint.name);
                p.active = true;
                p.metrics.reset();
                p.pacer.reset();
            }
        }
    }
}

/// Snapshot of a path's stats.
#[derive(Debug, Clone)]
pub struct PathStat {
    pub id: PathId,
    pub name: String,
    pub ip: String,
    pub transport: sp_common::config::TransportType,
    pub active: bool,
    pub avg_latency_ms: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub failure_rate: f64,
}
