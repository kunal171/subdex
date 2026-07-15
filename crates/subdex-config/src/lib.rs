//! # subdex-config
//!
//! A small, typed configuration loader so indexers don't each hand-roll the same
//! `std::env::var` parsing. It layers two sources, **env overriding file**:
//!
//! 1. an optional **TOML file** (`[source] / [store] / [processor]` tables), then
//! 2. **environment variables** (which win), so deployments can override any
//!    single value without editing the file.
//!
//! It then **validates** (naming the offending key on error) and builds the
//! framework's typed configs — [`SourceConfig`], [`StoreConfig`],
//! [`ProcessorConfig`] — so a binary is just:
//!
//! ```no_run
//! # fn main() -> Result<(), subdex_config::ConfigError> {
//! let cfg = subdex_config::IndexerConfig::load()?; // .env + env + optional file
//! let source_cfg = cfg.source_config();
//! let store_cfg = cfg.store_config();
//! let processor_cfg = cfg.processor_config();
//! # Ok(()) }
//! ```
//!
//! ## Environment variables
//!
//! | Var                 | Section.field         | Required |
//! |---------------------|-----------------------|----------|
//! | `WS_URL`            | `source.url`          | **yes**  |
//! | `DATABASE_URL`      | `store.url`           | **yes**  |
//! | `BATCH_SIZE`        | `source.batch_size` + `processor.batch_size` | no |
//! | `CONCURRENCY`       | `source.concurrency`  | no       |
//! | `SS58_PREFIX`       | `source.ss58_prefix`  | no       |
//! | `STRICT`            | `source.strict`       | no       |
//! | `MAX_CONNECTIONS`   | `store.max_connections` | no     |
//! | `REORG_RETENTION`   | `store.reorg_retention` | no     |
//! | `START_HEIGHT`      | `processor.start_height` | no    |
//! | `MAX_REORG_DEPTH`   | `processor.max_reorg_depth` | no |
//! | `SUBDEX_CONFIG`     | path to the TOML file | no (default `subdex.toml` if present) |

use serde::Deserialize;
use subdex::ProcessorConfig;
use subdex_source::SourceConfig;
use subdex_store::StoreConfig;
use thiserror::Error;

/// Errors from loading or validating configuration. Each names the offending
/// input so a misconfiguration is diagnosable at a glance.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The TOML file existed but couldn't be read.
    #[error("reading config file `{path}`: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    /// The TOML file didn't parse.
    #[error("parsing config file `{path}`: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    /// An environment variable held a value that didn't parse to the field type.
    #[error("env var `{key}` = {value:?} is not a valid {expected}")]
    BadEnv {
        key: String,
        value: String,
        expected: &'static str,
    },
    /// A required value was absent from both the file and the environment.
    #[error(
        "missing required config `{field}` (set env `{env}` or `[{section}]` in the config file)"
    )]
    Missing {
        field: &'static str,
        env: &'static str,
        section: &'static str,
    },
    /// A value was present but out of range / inconsistent.
    #[error("invalid config `{field}`: {reason}")]
    Invalid { field: &'static str, reason: String },
}

/// The `[source]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourceSection {
    pub url: Option<String>,
    pub batch_size: Option<u32>,
    pub concurrency: Option<usize>,
    pub ss58_prefix: Option<u16>,
    pub strict: Option<bool>,
}

/// The `[store]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreSection {
    pub url: Option<String>,
    pub max_connections: Option<u32>,
    pub reorg_retention: Option<u32>,
}

/// The `[processor]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessorSection {
    pub start_height: Option<u32>,
    pub batch_size: Option<u32>,
    pub max_reorg_depth: Option<u32>,
}

/// A fully-resolved indexer configuration, after layering file + env.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexerConfig {
    pub source: SourceSection,
    pub store: StoreSection,
    pub processor: ProcessorSection,
}

impl IndexerConfig {
    /// Load config: a local `.env` (if present) is applied to the process env
    /// first, then the TOML file (from `SUBDEX_CONFIG`, or `subdex.toml` if it
    /// exists), then environment variables override it. Finally, required fields
    /// are validated.
    pub fn load() -> Result<Self, ConfigError> {
        // Best-effort .env; real env vars still win.
        let _ = dotenvy::dotenv();
        let path = std::env::var("SUBDEX_CONFIG").ok().or_else(|| {
            std::path::Path::new("subdex.toml")
                .exists()
                .then(|| "subdex.toml".into())
        });
        Self::load_from(path.as_deref(), env_lookup)
    }

    /// Testable core: load from an optional file path + an env lookup fn.
    fn load_from(
        path: Option<&str>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let mut cfg = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|source| ConfigError::Read {
                    path: p.to_string(),
                    source,
                })?;
                toml::from_str(&text).map_err(|source| ConfigError::Parse {
                    path: p.to_string(),
                    source,
                })?
            }
            None => Self::default(),
        };
        cfg.overlay_env(&env)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Apply environment overrides on top of whatever the file provided.
    fn overlay_env(&mut self, env: &impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        if let Some(v) = env("WS_URL") {
            self.source.url = Some(v);
        }
        if let Some(v) = env("DATABASE_URL") {
            self.store.url = Some(v);
        }
        // Numeric / bool knobs, each parsed with a key-naming error.
        set_opt(&mut self.source.batch_size, env, "BATCH_SIZE", "u32")?;
        set_opt(&mut self.source.concurrency, env, "CONCURRENCY", "usize")?;
        set_opt(&mut self.source.ss58_prefix, env, "SS58_PREFIX", "u16")?;
        set_opt(&mut self.source.strict, env, "STRICT", "bool")?;
        set_opt(
            &mut self.store.max_connections,
            env,
            "MAX_CONNECTIONS",
            "u32",
        )?;
        set_opt(
            &mut self.store.reorg_retention,
            env,
            "REORG_RETENTION",
            "u32",
        )?;
        set_opt(&mut self.processor.start_height, env, "START_HEIGHT", "u32")?;
        set_opt(
            &mut self.processor.max_reorg_depth,
            env,
            "MAX_REORG_DEPTH",
            "u32",
        )?;
        // BATCH_SIZE also feeds the processor's batch size unless it set its own.
        if self.processor.batch_size.is_none() {
            self.processor.batch_size = self.source.batch_size;
        }
        Ok(())
    }

    /// Check required fields and cross-field constraints.
    fn validate(&self) -> Result<(), ConfigError> {
        if self.source.url.is_none() {
            return Err(ConfigError::Missing {
                field: "source.url",
                env: "WS_URL",
                section: "source",
            });
        }
        if self.store.url.is_none() {
            return Err(ConfigError::Missing {
                field: "store.url",
                env: "DATABASE_URL",
                section: "store",
            });
        }
        // reorg_retention (0 = keep all) must not silently be below max_reorg_depth,
        // or a reorg's fork point could be pruned out from under the walk.
        let retention = self.store.reorg_retention.unwrap_or(5000);
        let depth = self.processor.max_reorg_depth.unwrap_or(64);
        if retention != 0 && retention < depth {
            return Err(ConfigError::Invalid {
                field: "store.reorg_retention",
                reason: format!(
                    "must be >= processor.max_reorg_depth ({depth}) so a reorg's \
                     fork point stays in the table, or 0 to disable pruning; got {retention}"
                ),
            });
        }
        Ok(())
    }

    /// Build a [`SourceConfig`]. Panics only if `validate` was bypassed and the
    /// URL is missing (it isn't, via [`load`](Self::load)).
    pub fn source_config(&self) -> SourceConfig {
        let mut c = SourceConfig::new(self.source.url.clone().expect("validated: source.url"));
        if let Some(v) = self.source.batch_size {
            c = c.with_batch_size(v);
        }
        if let Some(v) = self.source.concurrency {
            c = c.with_concurrency(v);
        }
        if let Some(v) = self.source.ss58_prefix {
            c = c.with_ss58_prefix(v);
        }
        if let Some(v) = self.source.strict {
            c = c.with_strict(v);
        }
        c
    }

    /// Build a [`StoreConfig`].
    pub fn store_config(&self) -> StoreConfig {
        let mut c = StoreConfig::new(self.store.url.clone().expect("validated: store.url"));
        if let Some(v) = self.store.max_connections {
            c = c.with_max_connections(v);
        }
        if let Some(v) = self.store.reorg_retention {
            c = c.with_reorg_retention(v);
        }
        c
    }

    /// Build a [`ProcessorConfig`].
    pub fn processor_config(&self) -> ProcessorConfig {
        let mut c = ProcessorConfig::from_height(self.processor.start_height.unwrap_or(0));
        if let Some(v) = self.processor.batch_size {
            c = c.with_batch_size(v);
        }
        if let Some(v) = self.processor.max_reorg_depth {
            c = c.with_max_reorg_depth(v);
        }
        c
    }
}

/// Real environment lookup.
fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Parse an env var into `T`, setting `slot` only if the var is present. Absent
/// leaves the file value untouched; a present-but-unparseable value is an error
/// that names the key.
fn set_opt<T: std::str::FromStr>(
    slot: &mut Option<T>,
    env: &impl Fn(&str) -> Option<String>,
    key: &str,
    expected: &'static str,
) -> Result<(), ConfigError> {
    if let Some(raw) = env(key) {
        let parsed = raw.parse::<T>().map_err(|_| ConfigError::BadEnv {
            key: key.to_string(),
            value: raw.clone(),
            expected,
        })?;
        *slot = Some(parsed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build an env lookup from a map (test double for the process env).
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn env_only_builds_valid_config() {
        let cfg = IndexerConfig::load_from(
            None,
            env_of(&[
                ("WS_URL", "wss://node:9944"),
                ("DATABASE_URL", "postgres://localhost/subdex"),
                ("BATCH_SIZE", "300"),
                ("CONCURRENCY", "8"),
                ("SS58_PREFIX", "2"),
                ("STRICT", "true"),
            ]),
        )
        .expect("valid");

        let s = cfg.source_config();
        assert_eq!(s.url, "wss://node:9944");
        assert_eq!(s.batch_size, 300);
        assert_eq!(s.concurrency, 8);
        assert_eq!(s.ss58_prefix, 2);
        assert!(s.strict);
        assert_eq!(cfg.store_config().url, "postgres://localhost/subdex");
        // BATCH_SIZE also fed the processor batch size.
        assert_eq!(cfg.processor_config().batch_size, 300);
    }

    #[test]
    fn env_overrides_file() {
        // File sets batch_size 100; env overrides to 500.
        let toml = "\
[source]
url = \"wss://from-file:9944\"
batch_size = 100
[store]
url = \"postgres://from-file/db\"
";
        // Write to a temp file.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("subdex-cfg-test-{}.toml", std::process::id()));
        std::fs::write(&path, toml).unwrap();

        let cfg = IndexerConfig::load_from(
            path.to_str(),
            env_of(&[("BATCH_SIZE", "500")]), // override, WS_URL/DATABASE_URL from file
        )
        .expect("valid");
        std::fs::remove_file(&path).ok();

        assert_eq!(
            cfg.source_config().url,
            "wss://from-file:9944",
            "url from file"
        );
        assert_eq!(
            cfg.source_config().batch_size,
            500,
            "batch_size overridden by env"
        );
    }

    #[test]
    fn missing_required_names_the_key() {
        let err = IndexerConfig::load_from(None, env_of(&[("WS_URL", "wss://x")]))
            .expect_err("DATABASE_URL missing");
        match err {
            ConfigError::Missing { field, env, .. } => {
                assert_eq!(field, "store.url");
                assert_eq!(env, "DATABASE_URL");
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn bad_env_value_names_the_key() {
        let err = IndexerConfig::load_from(
            None,
            env_of(&[
                ("WS_URL", "wss://x"),
                ("DATABASE_URL", "postgres://x"),
                ("BATCH_SIZE", "not-a-number"),
            ]),
        )
        .expect_err("BATCH_SIZE unparseable");
        match err {
            ConfigError::BadEnv { key, expected, .. } => {
                assert_eq!(key, "BATCH_SIZE");
                assert_eq!(expected, "u32");
            }
            other => panic!("expected BadEnv, got {other:?}"),
        }
    }

    #[test]
    fn retention_below_reorg_depth_is_rejected() {
        let err = IndexerConfig::load_from(
            None,
            env_of(&[
                ("WS_URL", "wss://x"),
                ("DATABASE_URL", "postgres://x"),
                ("REORG_RETENTION", "10"),
                ("MAX_REORG_DEPTH", "64"),
            ]),
        )
        .expect_err("retention < depth");
        assert!(
            matches!(err, ConfigError::Invalid { field, .. } if field == "store.reorg_retention")
        );
    }

    #[test]
    fn retention_zero_disables_the_check() {
        // 0 = keep all → allowed even though 0 < depth.
        IndexerConfig::load_from(
            None,
            env_of(&[
                ("WS_URL", "wss://x"),
                ("DATABASE_URL", "postgres://x"),
                ("REORG_RETENTION", "0"),
            ]),
        )
        .expect("retention 0 is valid");
    }

    #[test]
    fn unknown_toml_field_is_rejected() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("subdex-cfg-bad-{}.toml", std::process::id()));
        std::fs::write(&path, "[source]\nurl = \"x\"\nnope = 1\n").unwrap();
        let err = IndexerConfig::load_from(path.to_str(), env_of(&[])).expect_err("unknown field");
        std::fs::remove_file(&path).ok();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
