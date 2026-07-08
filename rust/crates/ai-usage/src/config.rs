//! Config + secret resolution. See `SPEC.md §8`.
//!
//! Secrets are **file-path based** (sops-nix / NixOS native). The config holds
//! PATHS to secret files, never secret values. Per provider, an API key resolves
//! in this order:
//!
//!   1. configured `api_key_path` (explicit path, e.g. `/run/secrets/openrouter-api-key`)
//!   2. conventional default `/run/secrets/<provider>-api-key`
//!
//! A NixOS host managed with sops-nix needs **zero** ai-usage config if its
//! secrets are named `<provider>-api-key`. Codex/Claude additionally read their
//! existing OAuth token files (`~/.codex/auth.json`, `~/.claude/.credentials.json`)
//! directly — the same file-path model, never env vars.
//!
//! Read values stay in memory for the request and are never persisted by this crate.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const DEFAULT_SECRETS_ROOT: &str = "/run/secrets";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: Providers,
    /// Default to live fetch; `offline` forces cache.
    #[serde(default)]
    pub offline: bool,
    /// Override the secrets root (default `/run/secrets`). Useful for tests or
    /// non-NixOS hosts that keep secrets elsewhere.
    #[serde(default, skip_serializing)]
    pub secrets_root: Option<PathBuf>,
    /// Path to an assembled provider-auth file (opencode `auth.json` shape:
    /// `{ "<entry>": { "type": "api", "key": "..." } }`). sops-nix hosts that
    /// already render this for opencode get all providers for free. Override via
    /// `opencode_auth_path` here or `OPENCODE_AUTH`.
    #[serde(default, skip_serializing)]
    pub opencode_auth_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Providers {
    #[serde(default)]
    pub openrouter: Option<ProviderSecret>,
    #[serde(default)]
    pub deepseek: Option<ProviderSecret>,
    #[serde(default)]
    pub zai: Option<ProviderSecret>,
    #[serde(default)]
    pub minimax: Option<ProviderSecret>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSecret {
    /// Path to a secret FILE (sops-nix `/run/secrets/...`). Never the value itself.
    #[serde(default)]
    pub api_key_path: Option<PathBuf>,
    /// Region/host override; must be HTTPS or resolution fails closed.
    #[serde(default)]
    pub api_host: Option<String>,
    /// Path to a cookie-session file (providers like MiniMax whose coding-plan
    /// quota API requires a logged-in browser session, not an API key). The file
    /// holds the raw `Cookie:` header text. Resolved like `api_key_path`.
    #[serde(default)]
    pub cookie_path: Option<PathBuf>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let mut cfg: Self = serde_json::from_str(&raw)
                .map_err(|e| Error::Config(format!("invalid config {}: {}", path.display(), e)))?;
            cfg.offline = cfg.offline || env_bool("AI_USAGE_OFFLINE");
            Ok(cfg)
        } else {
            Ok(Self {
                offline: env_bool("AI_USAGE_OFFLINE"),
                ..Default::default()
            })
        }
    }

    /// Resolve a provider API key. Order:
    ///   1. configured `api_key_path` (explicit secret file)
    ///   2. conventional `<secrets_root>/<provider>-api-key`
    ///   3. an assembled opencode-style `auth.json` at entry `opencode_entry`
    ///      (sops-nix renders this trevor-readable on hosts that already provision
    ///      opencode — reuses the sops-decrypted values with zero extra config)
    ///
    /// Returns `Ok(None)` (not an error) when no secret is readable — the caller
    /// reports `Unavailable{NoCredentials}`. Root-owned `/run/secrets/*` files
    /// that exist but are unreadable are skipped, not fatal.
    pub fn resolve_provider_key(
        &self,
        provider: &str,
        opencode_entry: &str,
        configured: Option<&Path>,
    ) -> Result<Option<String>> {
        if let Some(p) = configured {
            if let Some(v) = read_if_accessible(p)? {
                return Ok(Some(v));
            }
        }
        let root = self
            .secrets_root
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SECRETS_ROOT));
        let conventional = root.join(format!("{provider}-api-key"));
        if let Some(v) = read_if_accessible(&conventional)? {
            return Ok(Some(v));
        }
        Ok(self.read_opencode_key(opencode_entry))
    }

    /// Resolve a cookie-session file (providers whose quota API needs a browser
    /// session). Order: configured `cookie_path` → conventional
    /// `<secrets_root>/<provider>-cookie`. Returns `Ok(None)` when absent.
    pub fn resolve_cookie(
        &self,
        provider: &str,
        configured: Option<&Path>,
    ) -> Result<Option<String>> {
        let root = self
            .secrets_root
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SECRETS_ROOT));
        let conventional = root.join(format!("{provider}-cookie"));
        if let Some(p) = configured {
            if let Some(v) = read_if_accessible(p)? {
                return Ok(Some(v));
            }
        }
        read_if_accessible(&conventional)
    }

    /// Read a provider key from an assembled opencode-style `auth.json`.
    fn read_opencode_key(&self, entry: &str) -> Option<String> {
        let p = self
            .opencode_auth_path
            .clone()
            .or_else(|| std::env::var("OPENCODE_AUTH").ok().map(PathBuf::from))
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".local/share/opencode/auth.json"))
            });
        let path = p?;
        let raw = std::fs::read_to_string(&path).ok()?;
        let v = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
        v.get(entry)
            .and_then(|e| e.get("key"))
            .and_then(|k| k.as_str())
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string)
    }

    /// Resolve a provider API key by reading its secret FILE. Order:
    /// configured `api_key_path` → conventional `<secrets_root>/<provider>-api-key`.
    /// Returns `Ok(None)` (not an error) when no secret is provisioned — the
    /// caller reports `Unavailable{NoCredentials}`.
    pub fn resolve_secret_file(
        &self,
        provider: &str,
        configured: Option<&Path>,
    ) -> Result<Option<String>> {
        let root = self
            .secrets_root
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SECRETS_ROOT));
        let conventional = root.join(format!("{provider}-api-key"));

        if let Some(p) = configured {
            if let Some(v) = read_if_accessible(p)? {
                return Ok(Some(v));
            }
        }
        read_if_accessible(&conventional)
    }
}

/// Read a secret file if it exists AND is readable. Returns `Ok(None)` for
/// missing files and permission-denied (root-owned `/run/secrets/*` on a
/// sops-nix host) so resolution can fall through to the next source.
fn read_if_accessible(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(Error::Config(format!(
                    "secret file {} is empty",
                    path.display()
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(None),
        Err(e) => Err(Error::Config(format!(
            "read secret {}: {}",
            path.display(),
            e
        ))),
    }
}

fn config_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AI_USAGE_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    std::env::var("HOME").map_or_else(
        |_| {
            Err(Error::Config(
                "HOME not set and AI_USAGE_CONFIG unset".into(),
            ))
        },
        |home| Ok(PathBuf::from(home).join(".config/ai-usage/config.json")),
    )
}

fn env_bool(var: &str) -> bool {
    matches!(
        std::env::var(var).ok().as_deref(),
        Some("1" | "true" | "yes")
    )
}

/// Validate that a host/url override is HTTPS-only before any bearer token is
/// attached. Explicit `http://` fails closed.
pub fn assert_https(raw: &str) -> Result<()> {
    if raw.trim().starts_with("http://") {
        return Err(Error::Config(format!(
            "endpoint override must be HTTPS: {}",
            crate::redact::Redactor::redact_str(raw)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_override_fails_closed() {
        assert!(assert_https("http://evil.example.com").is_err());
        assert!(assert_https("https://api.openrouter.ai").is_ok());
    }

    #[test]
    fn secret_resolution_prefers_configured_path() {
        let tmp = std::env::temp_dir().join(format!("aiu-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let cfg_path = tmp.join("openrouter-api-key");
        let conv_path = tmp.join("ai-usage").join("openrouter-api-key");
        std::fs::write(&cfg_path, "sk-from-config-path\n").unwrap();
        std::fs::create_dir_all(conv_path.parent().unwrap()).unwrap();
        std::fs::write(&conv_path, "sk-from-conventional\n").unwrap();

        let cfg = Config {
            secrets_root: Some(tmp.join("ai-usage")),
            ..Default::default()
        };
        // configured path wins
        let v = cfg
            .resolve_secret_file("openrouter", Some(&cfg_path))
            .unwrap();
        assert_eq!(v.as_deref(), Some("sk-from-config-path"));
        // falls back to conventional
        let v = cfg.resolve_secret_file("openrouter", None).unwrap();
        assert_eq!(v.as_deref(), Some("sk-from-conventional"));
        // neither present → None, not an error
        let v = cfg.resolve_secret_file("deepseek", None).unwrap();
        assert_eq!(v, None);

        std::fs::remove_dir_all(&tmp).ok();
    }
}
