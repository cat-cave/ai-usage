//! Aggregate across providers + versioned JSON schema envelope.

use serde::{Deserialize, Serialize};

use crate::model::{ProviderId, ProviderReport};
use crate::recommend::{Recommendation, TaskKind};

/// JSON schema version. Bumped on any breaking change to `--json` output.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateReport {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub providers: Vec<ProviderReport>,
}

impl AggregateReport {
    pub const fn new(providers: Vec<ProviderReport>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            providers,
        }
    }

    pub fn find(&self, id: ProviderId) -> Option<&ProviderReport> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn recommend(&self, task: TaskKind) -> Vec<Recommendation> {
        crate::recommend::recommend(self, task)
    }
}
