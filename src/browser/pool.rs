use super::instance::BrowserInstance;
use crate::error::PdfError;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Circuit breaker state - combined into single struct to prevent deadlock from inconsistent lock ordering
struct CircuitBreakerState {
    consecutive_failures: usize,
    /// Timestamp when circuit breaker was last tripped (for half-open recovery)
    tripped_at: Option<Instant>,
}

impl CircuitBreakerState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            tripped_at: None,
        }
    }

    fn record_failure(&mut self, max_failures: usize) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= max_failures && self.tripped_at.is_none() {
            self.tripped_at = Some(Instant::now());
        }
    }

    fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.tripped_at = None;
    }

    fn is_open(&self, max_failures: usize) -> bool {
        self.consecutive_failures >= max_failures
    }

    fn should_attempt_recovery(&self, cooldown_secs: u64) -> bool {
        if let Some(trip_time) = self.tripped_at {
            trip_time.elapsed() >= Duration::from_secs(cooldown_secs)
        } else {
            false
        }
    }

    fn reset_cooldown(&mut self) {
        self.tripped_at = Some(Instant::now());
    }
}

/// Connection pool for browser instances
pub struct BrowserPool {
    instances: Arc<Mutex<VecDeque<Arc<BrowserInstance>>>>,
    chrome_path: String,
    max_instances: usize,
    min_instances: usize,
    /// Circuit breaker state - single lock to prevent deadlock
    circuit_breaker: Arc<Mutex<CircuitBreakerState>>,
}

impl BrowserPool {
    pub async fn new(
        chrome_path: String,
        min_instances: usize,
        max_instances: usize,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut instances = VecDeque::new();

        // Create initial instances
        for i in 0..min_instances {
            info!(
                current = i + 1,
                total = min_instances,
                "Creating browser instance"
            );
            match BrowserInstance::new(&chrome_path).await {
                Ok(instance) => instances.push_back(Arc::new(instance)),
                Err(e) => error!(error = %e, "Failed to create initial browser instance"),
            }
        }

        if instances.is_empty() {
            return Err("Failed to create any browser instances".into());
        }

        Ok(BrowserPool {
            instances: Arc::new(Mutex::new(instances)),
            chrome_path,
            max_instances,
            min_instances,
            circuit_breaker: Arc::new(Mutex::new(CircuitBreakerState::new())),
        })
    }

    /// Clears all instances from the pool (used during shutdown)
    pub async fn clear(&self) -> usize {
        let mut instances = self.instances.lock().await;
        let count = instances.len();
        instances.clear();
        count
    }

    pub async fn get_healthy_instance(&self) -> Result<Arc<BrowserInstance>, PdfError> {
        const MAX_RETRIES: usize = 3;

        for attempt in 0..MAX_RETRIES {
            let mut instances = self.instances.lock().await;

            // Remove unhealthy instances - collect indices to remove first
            let mut to_remove = Vec::new();
            for (idx, instance) in instances.iter().enumerate() {
                if !instance.is_healthy().await {
                    warn!("Removing unhealthy browser instance");
                    to_remove.push(idx);
                }
            }
            // Remove in reverse order to preserve indices
            for idx in to_remove.into_iter().rev() {
                instances.remove(idx);
            }

            // Try to find a healthy instance that shouldn't retire
            for instance in instances.iter() {
                if instance.is_healthy().await && !instance.should_retire().await {
                    // Pre-use health ping - verify browser is actually responsive
                    if !instance.health_ping().await {
                        warn!("Browser failed pre-use health ping, marking unhealthy");
                        instance.mark_unhealthy().await;
                        continue;
                    }
                    instance.mark_used().await;
                    // Reset circuit breaker on success
                    self.circuit_breaker.lock().await.reset();
                    return Ok(Arc::clone(instance));
                }
            }

            // Create new instance if needed and allowed
            if instances.len() < self.max_instances {
                info!(attempt = attempt + 1, "Creating new browser instance");
                match BrowserInstance::new(&self.chrome_path).await {
                    Ok(new_instance) => {
                        let instance_arc = Arc::new(new_instance);
                        instance_arc.mark_used().await;
                        instances.push_back(Arc::clone(&instance_arc));
                        // Reset circuit breaker on success
                        self.circuit_breaker.lock().await.reset();
                        return Ok(instance_arc);
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to create new browser instance");
                        // Record failure in circuit breaker
                        self.circuit_breaker.lock().await.record_failure(10);
                        if attempt < MAX_RETRIES - 1 {
                            // Wait before retry
                            drop(instances); // Release lock before sleeping
                            sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
                        }
                    }
                }
            } else if instances.is_empty() {
                // No instances available and can't create more
                return Err(PdfError::AllInstancesFailed);
            }
        }

        Err(PdfError::AllInstancesFailed)
    }

    pub async fn maintain(&self) {
        const MAX_CONSECUTIVE_FAILURES: usize = 10;
        const CIRCUIT_BREAKER_RECOVERY_SECS: u64 = 120; // 2 minutes before attempting recovery

        // Check circuit breaker state - acquire lock once and release before any async work
        {
            let cb = self.circuit_breaker.lock().await;
            if cb.is_open(MAX_CONSECUTIVE_FAILURES) {
                let pool_size = self.instances.lock().await.len();

                if !cb.should_attempt_recovery(CIRCUIT_BREAKER_RECOVERY_SECS) {
                    // Still in cooldown period, skip maintenance
                    let remaining = if let Some(trip_time) = cb.tripped_at {
                        CIRCUIT_BREAKER_RECOVERY_SECS.saturating_sub(trip_time.elapsed().as_secs())
                    } else {
                        CIRCUIT_BREAKER_RECOVERY_SECS
                    };
                    warn!(
                        failures = cb.consecutive_failures,
                        pool_size = pool_size,
                        recovery_in_secs = remaining,
                        "Maintenance: circuit breaker open"
                    );

                    // If no trip time recorded, set it now
                    if cb.tripped_at.is_none() {
                        drop(cb);
                        self.circuit_breaker.lock().await.reset_cooldown();
                    }
                    return;
                }

                // Half-open state: will attempt recovery after releasing lock
                let elapsed = cb.tripped_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                drop(cb);

                info!(
                    cooldown_secs = elapsed,
                    "Maintenance: circuit breaker half-open, attempting recovery"
                );

                match BrowserInstance::new(&self.chrome_path).await {
                    Ok(new_instance) => {
                        let mut instances = self.instances.lock().await;
                        instances.push_back(Arc::new(new_instance));
                        drop(instances);
                        // Reset circuit breaker on success
                        self.circuit_breaker.lock().await.reset();
                        info!("Maintenance: circuit breaker RESET - recovery successful!");
                        return;
                    }
                    Err(e) => {
                        error!(error = %e, "Maintenance: half-open recovery failed");
                        // Reset the trip time to start a new cooldown period
                        self.circuit_breaker.lock().await.reset_cooldown();

                        // Fatal crash logic for self-healing
                        error!("Fatal: Failed to recover browser pool from half-open state. Initiating self-healing restart.");
                        std::process::exit(1);
                    }
                }
            }
        }

        let mut instances = self.instances.lock().await;

        // Remove unhealthy, old, and idle instances - collect indices first
        let initial_count = instances.len();
        let mut to_remove = Vec::new();
        for (idx, instance) in instances.iter().enumerate() {
            let is_unhealthy = !instance.is_healthy().await;
            let should_retire = instance.should_retire().await;
            // Only remove idle instances if we're above minimum pool size
            let is_idle = instances.len() > self.min_instances && instance.is_idle_too_long().await;

            if is_unhealthy || should_retire || is_idle {
                if is_unhealthy {
                    debug!("Marking instance for removal: unhealthy");
                } else if should_retire {
                    debug!("Marking instance for removal: should retire (age/uses)");
                } else if is_idle {
                    debug!("Marking instance for removal: idle too long");
                }
                to_remove.push(idx);
            }
        }
        // Remove in reverse order to preserve indices
        for idx in to_remove.into_iter().rev() {
            instances.remove(idx);
        }

        let removed = initial_count - instances.len();
        if removed > 0 {
            info!(
                count = removed,
                "Maintenance: removed unhealthy/old/idle browser instances"
            );
        }

        // Ensure minimum instances with circuit breaker
        let mut maintenance_failures = 0;
        const MAX_MAINTENANCE_ATTEMPTS: usize = 3;
        drop(instances); // Release lock before creating instances

        loop {
            // Check current pool size
            let current_len = self.instances.lock().await.len();

            if current_len >= self.min_instances || maintenance_failures >= MAX_MAINTENANCE_ATTEMPTS
            {
                break;
            }

            match BrowserInstance::new(&self.chrome_path).await {
                Ok(new_instance) => {
                    let mut instances = self.instances.lock().await;
                    instances.push_back(Arc::new(new_instance));
                    info!("Maintenance: added new browser instance");
                    drop(instances);
                    // Reset circuit breaker on success
                    self.circuit_breaker.lock().await.reset();
                    maintenance_failures = 0; // Reset local counter too
                }
                Err(e) => {
                    error!(error = %e, "Maintenance: failed to create browser instance");
                    maintenance_failures += 1;
                    // Record failure in circuit breaker
                    self.circuit_breaker
                        .lock()
                        .await
                        .record_failure(MAX_CONSECUTIVE_FAILURES);

                    if maintenance_failures >= MAX_MAINTENANCE_ATTEMPTS {
                        let current_pool_size = self.instances.lock().await.len();
                        error!(
                            failures = maintenance_failures,
                            pool_size = current_pool_size,
                            "Maintenance: stopping after consecutive failures"
                        );
                    }
                    break;
                }
            }
        }
    }
}
