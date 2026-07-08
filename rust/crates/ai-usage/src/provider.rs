//! The `Provider` trait, `Registry`, and resolver glue.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{self, Result};
use crate::model::{ProviderId, ProviderReport};
use crate::source::UnavailableReason;

/// One provider adapter. `fetch` is async (reqwest/rustls at the edge). Domain
/// math stays sync and pure. Implementations never print; they return data or errors.
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// True if this provider has the credentials/config needed to attempt a live
    /// fetch. Used by `doctor` and to decide between `Unavailable{NoCredentials}`
    /// and an actual attempt.
    fn configured(&self) -> bool;

    async fn fetch(&self) -> Result<ProviderReport>;
}

/// Resolves the set of providers from the environment + config file.
pub struct Registry {
    providers: Vec<Arc<dyn Provider>>,
}

impl Registry {
    pub fn from_env() -> Result<Self> {
        let cfg = crate::config::Config::load()?;
        let providers: Vec<Arc<dyn Provider>> = vec![
            Arc::new(crate::providers::codex::CodexProvider::from_config(&cfg)?),
            Arc::new(crate::providers::claude::ClaudeProvider::from_config(&cfg)?),
            Arc::new(crate::providers::zai::ZaiProvider::from_config(&cfg)?),
            Arc::new(crate::providers::minimax::MiniMaxProvider::from_config(
                &cfg,
            )?),
            Arc::new(crate::providers::openrouter::OpenRouterProvider::from_config(&cfg)?),
            Arc::new(crate::providers::deepseek::DeepSeekProvider::from_config(
                &cfg,
            )?),
            Arc::new(crate::providers::grok::GrokProvider::from_config(&cfg)?),
        ];
        Ok(Self { providers })
    }

    pub fn ids(&self) -> Vec<ProviderId> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    /// Fetch every provider concurrently.
    pub async fn snapshot(&self) -> crate::report::AggregateReport {
        let mut handles = Vec::with_capacity(self.providers.len());
        for p in &self.providers {
            let id = p.id();
            let p = Arc::clone(p);
            handles.push(tokio::spawn(async move { (id, p.fetch().await) }));
        }
        let mut reports = Vec::with_capacity(handles.len());
        for h in handles {
            let (id, res) = h.await.unwrap_or_else(|e| {
                (
                    ProviderId::OpenRouter,
                    Err(crate::error::Error::Http(format!("join: {e}"))),
                )
            });
            reports.push(match res {
                Ok(r) => r,
                Err(e) => error_report(id, &e),
            });
        }
        crate::report::AggregateReport::new(reports)
    }
}

fn error_report(id: ProviderId, e: &crate::error::Error) -> ProviderReport {
    use crate::source::unavailable_field as u;
    ProviderReport {
        id,
        account: u(UnavailableReason::Network),
        windows: vec![],
        banked_resets: vec![],
        plan_capacity: u(UnavailableReason::Network),
        paid_overflow: u(UnavailableReason::Network),
        api_balance: u(UnavailableReason::Network),
        local_velocity: u(UnavailableReason::Network),
        status: crate::model::ProviderStatus::Unavailable {
            reason: error::safe_reason(e),
        },
    }
}
