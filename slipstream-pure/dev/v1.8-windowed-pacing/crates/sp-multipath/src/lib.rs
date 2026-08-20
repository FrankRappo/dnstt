pub mod metrics;
pub mod pacing;

use anyhow::Result;
use sp_common::config::ResolverEndpoint;
use sp_transport::{DnsQuery, DnsResponse, Transport};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

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
    /// Downstream delivery tracking (separate from transport-level metrics).
    /// Counts how many times this path delivered non-empty TXT data vs empty.
    pub ds_success: u64,
    pub ds_empty: u64,
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
/// Tracks "good paths" — those confirmed to deliver downstream TXT data.
pub struct MultipathRouter {
    paths: Arc<RwLock<Vec<VirtualPath>>>,
    next_path_id: std::sync::atomic::AtomicU64,
    /// Round-robin index for all paths
    rr_index: std::sync::atomic::AtomicUsize,
    /// Paths with confirmed downstream TXT delivery
    good_paths: Arc<RwLock<Vec<PathId>>>,
    /// Round-robin index for good paths
    good_rr_index: std::sync::atomic::AtomicUsize,
}

impl MultipathRouter {
    pub fn new() -> Self {
        Self {
            paths: Arc::new(RwLock::new(Vec::new())),
            next_path_id: std::sync::atomic::AtomicU64::new(0),
            rr_index: std::sync::atomic::AtomicUsize::new(0),
            good_paths: Arc::new(RwLock::new(Vec::new())),
            good_rr_index: std::sync::atomic::AtomicUsize::new(0),
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
            ds_success: 0,
            ds_empty: 0,
        };
        self.paths.write().await.push(path);
        debug!("added path {}", id);
        id
    }

    /// Remove a path by ID.
    pub async fn remove_path(&self, id: PathId) {
        let mut paths = self.paths.write().await;
        paths.retain(|p| p.id != id);
        drop(paths);
        self.good_paths.write().await.retain(|&pid| pid != id);
        debug!("removed path {}", id);
    }

    /// Get count of active paths.
    pub async fn active_path_count(&self) -> usize {
        self.paths.read().await.iter().filter(|p| p.active).count()
    }

    /// Get count of good paths (confirmed downstream delivery).
    pub async fn good_path_count(&self) -> usize {
        self.good_paths.read().await.len()
    }

    /// Send a DNS query through the best available path (round-robin).
    /// Used for upstream data where any path works.
    pub async fn send(&self, query: &DnsQuery) -> Result<(DnsResponse, PathId)> {
        let paths = self.paths.read().await;
        let active: Vec<usize> = paths
            .iter()
            .enumerate()
            .filter(|(_, p)| p.active && p.pacer.can_send())
            .map(|(i, _)| i)
            .collect();

        if active.is_empty() {
            drop(paths);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            return Err(anyhow::anyhow!("all paths are rate-limited or inactive"));
        }

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

        let mut paths = self.paths.write().await;
        if let Some(path) = paths.iter_mut().find(|p| p.id == path_id) {
            match &result {
                Ok(_) => {
                    path.metrics.record_success(elapsed);
                    path.pacer.on_success();
                }
                Err(_e) => {
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

    /// Send through a specific path.
    pub async fn send_via(&self, query: &DnsQuery, path_id: PathId) -> Result<(DnsResponse, PathId)> {
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

        result.map(|r| (r, path_id))
    }

    /// Send through good paths only (for downstream polling).
    /// Falls back to all paths if no good paths are known yet.
    pub async fn send_via_good_paths(&self, query: &DnsQuery) -> Result<(DnsResponse, PathId)> {
        let good = self.good_paths.read().await;
        if good.is_empty() {
            drop(good);
            return self.send(query).await;
        }

        let idx = self
            .good_rr_index
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % good.len();
        let path_id = good[idx];
        drop(good);

        match self.send_via(query, path_id).await {
            Ok(resp) => Ok(resp),
            Err(_) => {
                // Good path failed at transport level, try regular send
                self.send(query).await
            }
        }
    }

    /// Send through N good paths in parallel, return first successful response.
    /// Falls back to regular send if no good paths are known.
    pub async fn send_parallel_good(
        &self,
        query: &DnsQuery,
        count: usize,
    ) -> Result<(DnsResponse, PathId)> {
        let good = self.good_paths.read().await;
        if good.is_empty() {
            drop(good);
            return self.send(query).await;
        }

        let n = count.min(good.len());
        let start_idx = self
            .good_rr_index
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        let selected: Vec<PathId> = (0..n)
            .map(|i| good[(start_idx + i) % good.len()])
            .collect();
        drop(good);

        // Gather transports for selected paths
        let paths = self.paths.read().await;
        let transports: Vec<(PathId, Arc<dyn Transport>)> = selected
            .iter()
            .filter_map(|&pid| {
                paths
                    .iter()
                    .find(|p| p.id == pid && p.active)
                    .map(|p| (p.id, p.transport.clone()))
            })
            .collect();
        drop(paths);

        if transports.is_empty() {
            return self.send(query).await;
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(transports.len());
        for (pid, transport) in &transports {
            let q = query.clone();
            let tx = tx.clone();
            let pid = *pid;
            let transport = transport.clone();
            tokio::spawn(async move {
                if let Ok(resp) = transport.exchange(&q).await {
                    let _ = tx.send((resp, pid)).await;
                }
            });
        }
        drop(tx);

        match rx.recv().await {
            Some((resp, pid)) => {
                // Update metrics for the winning path
                let mut paths = self.paths.write().await;
                if let Some(path) = paths.iter_mut().find(|p| p.id == pid) {
                    path.metrics.record_success(std::time::Duration::from_millis(50));
                    path.pacer.on_success();
                }
                Ok((resp, pid))
            }
            None => {
                // All parallel paths failed, try regular send
                self.send(query).await
            }
        }
    }

    /// Report that a path successfully delivered downstream TXT data.
    /// Adds the path to the good_paths list if not already there.
    pub async fn report_downstream_success(&self, path_id: PathId) {
        // Update downstream counters
        {
            let mut paths = self.paths.write().await;
            if let Some(path) = paths.iter_mut().find(|p| p.id == path_id) {
                path.ds_success += 1;
            }
        }

        // Add to good paths if not already present
        let mut good = self.good_paths.write().await;
        if !good.contains(&path_id) {
            good.push(path_id);
            info!("path {} added to good paths ({} total)", path_id, good.len());
        }
    }

    /// Report that a path returned empty TXT when data was expected.
    /// Only deactivates if downstream failure rate > 90% with at least 10 attempts.
    pub async fn report_downstream_empty(&self, path_id: PathId) {
        let mut paths = self.paths.write().await;
        if let Some(path) = paths.iter_mut().find(|p| p.id == path_id) {
            path.ds_empty += 1;
            let total = path.ds_success + path.ds_empty;
            let failure_rate = path.ds_empty as f64 / total as f64;
            if failure_rate > 0.9 && total >= 10 {
                warn!(
                    "path {} ({}) downstream empty rate {:.0}%, removing from good paths",
                    path.id,
                    path.endpoint.name,
                    failure_rate * 100.0
                );
                // Remove from good paths but don't deactivate the path entirely
                // (it can still be used for upstream)
                drop(paths);
                self.good_paths
                    .write()
                    .await
                    .retain(|&pid| pid != path_id);
            }
        }
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

    /// Discover good paths by sending a query to ALL active paths in parallel.
    /// Paths that return non-empty TXT data are marked as good.
    /// Returns the number of newly discovered good paths.
    pub async fn discover_good_paths(&self, query: &DnsQuery) -> usize {
        let paths = self.paths.read().await;
        let transports: Vec<(PathId, Arc<dyn Transport>)> = paths
            .iter()
            .filter(|p| p.active)
            .map(|p| (p.id, p.transport.clone()))
            .collect();
        drop(paths);

        if transports.is_empty() {
            return 0;
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(transports.len());
        for (pid, transport) in transports {
            let q = query.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    transport.exchange(&q),
                )
                .await
                {
                    Ok(Ok(resp)) => {
                        let _ = tx.send((pid, resp)).await;
                    }
                    _ => {}
                }
            });
        }
        drop(tx);

        let mut count = 0;
        while let Some((pid, resp)) = rx.recv().await {
            let has_data = resp.txt_data.as_ref().map_or(false, |d| !d.is_empty());
            if has_data {
                self.report_downstream_success(pid).await;
                count += 1;
            }
        }
        count
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
    /// Also resets downstream counters but preserves good_paths list.
    pub async fn reactivate_all(&self) {
        let mut paths = self.paths.write().await;
        for p in paths.iter_mut() {
            if !p.active {
                debug!("reactivating path {} ({})", p.id, p.endpoint.name);
                p.active = true;
                p.metrics.reset();
                p.pacer.reset();
            }
            // Reset downstream counters to allow paths to re-prove themselves
            p.ds_success = 0;
            p.ds_empty = 0;
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
