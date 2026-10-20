use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Provider {
    pub preset: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    #[default]
    Fallback,
    RoundRobin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Daemon {
    pub socket: Option<PathBuf>,
    pub listen: Option<std::net::SocketAddr>,
    pub token_env: Option<String>,
    pub max_body_bytes: usize,
}
impl Default for Daemon {
    fn default() -> Self {
        Self {
            socket: None,
            listen: None,
            token_env: None,
            max_body_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub provider: String,
    pub fallback: Vec<String>,
    pub strategy: Strategy,
    pub concurrency: usize,
    pub timeout_secs: u64,
    pub retries: u32,
    pub providers: BTreeMap<String, Provider>,
    pub daemon: Daemon,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "jev".into(),
            fallback: vec![],
            strategy: Strategy::Fallback,
            concurrency: 8,
            timeout_secs: 60,
            retries: 2,
            providers: BTreeMap::new(),
            daemon: Daemon::default(),
        }
    }
}

#[derive(Clone)]
pub struct ResolvedProvider {
    pub name: String,
    pub base_url: String,
    pub model: Option<String>,
    pub api_key_env: String,
}

pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sq/config.toml")
}

pub fn default_socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(dirs::runtime_dir)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("sq")
        })
        .join("sq.sock")
}

pub fn origin(value: &str) -> Result<String> {
    let url = url::Url::parse(value).context("base_url must be an absolute HTTP(S) origin")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "base_url needs HTTP(S) and a host"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "base_url must not contain a path, credentials, query, or fragment"
    );
    Ok(url.origin().ascii_serialization())
}

impl Config {
    pub fn load(path: Option<&std::path::Path>) -> Result<Self> {
        let optional = path.is_none();
        let default = default_path();
        let path = path.unwrap_or(&default);
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s)
                .with_context(|| format!("invalid TOML config: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && optional => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read config {}", path.display())),
        }
    }

    pub fn resolve(&self, name: &str) -> Result<ResolvedProvider> {
        let p = self.providers.get(name).cloned().unwrap_or_default();
        let preset = p.preset.as_deref().unwrap_or(name);
        let (base, model, key) = match preset {
            "jev" | "typesafe" => (
                "https://api.typesafe.ai",
                Some("jev-latest"),
                "TYPESAFE_API_KEY",
            ),
            "drex" => (
                "https://console.nace.ai",
                Some("drex-latest"),
                "NACE_API_KEY",
            ),
            "laya" => ("http://127.0.0.1:8000", None, "LAYA_API_KEY"),
            "custom" => ("", None, "SQ_API_KEY"),
            _ if self.providers.contains_key(name) && p.preset.is_none() => {
                ("", None, "SQ_API_KEY")
            }
            _ => {
                bail!("unknown provider {name}; use jev, drex, laya, or define [providers.{name}]")
            }
        };
        let base_url = origin(p.base_url.as_deref().unwrap_or(base))?;
        let model = p.model.or_else(|| model.map(str::to_string));
        ensure!(
            model.as_ref().is_none_or(|m| !m.trim().is_empty()),
            "provider model must not be blank"
        );
        let api_key_env = p.api_key_env.unwrap_or_else(|| key.into());
        ensure!(
            !api_key_env.is_empty()
                && api_key_env
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "api_key_env must be an environment variable name"
        );
        Ok(ResolvedProvider {
            name: name.into(),
            base_url,
            model,
            api_key_env,
        })
    }

    pub fn validate(&self) -> Result<Vec<ResolvedProvider>> {
        ensure!(
            (1..=1024).contains(&self.concurrency),
            "concurrency must be 1..1024"
        );
        ensure!(
            (1..=3600).contains(&self.timeout_secs),
            "timeout_secs must be 1..3600"
        );
        ensure!(self.retries <= 5, "retries must be 0..5");
        ensure!(
            (1024..=64 * 1024 * 1024).contains(&self.daemon.max_body_bytes),
            "daemon max_body_bytes must be 1024..67108864"
        );
        let mut seen = BTreeSet::new();
        std::iter::once(&self.provider)
            .chain(&self.fallback)
            .map(|name| {
                ensure!(seen.insert(name), "duplicate provider in pool: {name}");
                self.resolve(name)
            })
            .collect()
    }
}
