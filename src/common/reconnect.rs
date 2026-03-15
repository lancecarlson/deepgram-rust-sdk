//! Reconnection configuration for WebSocket connections.

use std::time::Duration;

/// Configuration for automatic WebSocket reconnection with exponential backoff.
///
/// When enabled, if a WebSocket connection drops unexpectedly (not from a
/// user-initiated close), the client will automatically attempt to reconnect.
///
/// Reconnection is disabled by default for backward compatibility.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Maximum number of reconnection attempts. `None` means unlimited.
    pub max_retries: Option<u32>,
    /// Initial delay before the first reconnection attempt.
    pub initial_delay: Duration,
    /// Maximum delay between reconnection attempts.
    pub max_delay: Duration,
    /// Multiplier for exponential backoff.
    pub backoff_multiplier: f64,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_retries: Some(5),
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(16),
            backoff_multiplier: 2.0,
        }
    }
}

impl ReconnectConfig {
    /// Create a new reconnection config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum number of retry attempts.
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = Some(max_retries);
        self
    }

    /// Set unlimited retry attempts.
    pub fn unlimited_retries(mut self) -> Self {
        self.max_retries = None;
        self
    }

    /// Set the initial delay before reconnection.
    pub fn initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Set the maximum delay between attempts.
    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Set the backoff multiplier.
    pub fn backoff_multiplier(mut self, multiplier: f64) -> Self {
        self.backoff_multiplier = multiplier;
        self
    }

    /// Calculate the delay for a given attempt number (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay_ms =
            self.initial_delay.as_millis() as f64 * self.backoff_multiplier.powi(attempt as i32);
        let capped_ms = delay_ms.min(self.max_delay.as_millis() as f64) as u64;
        Duration::from_millis(capped_ms)
    }

    /// Check if another attempt should be made given the current attempt count.
    pub fn should_retry(&self, attempt: u32) -> bool {
        match self.max_retries {
            Some(max) => attempt < max,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ReconnectConfig::default();
        assert_eq!(config.max_retries, Some(5));
        assert!(config.should_retry(0));
        assert!(config.should_retry(4));
        assert!(!config.should_retry(5));
    }

    #[test]
    fn test_backoff_delay() {
        let config = ReconnectConfig::new()
            .initial_delay(Duration::from_millis(100))
            .backoff_multiplier(2.0)
            .max_delay(Duration::from_secs(5));

        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(config.delay_for_attempt(2), Duration::from_millis(400));
        // Should cap at max_delay
        assert_eq!(config.delay_for_attempt(20), Duration::from_secs(5));
    }

    #[test]
    fn test_unlimited_retries() {
        let config = ReconnectConfig::new().unlimited_retries();
        assert!(config.should_retry(1000));
    }
}
