use serde::Deserialize;
use std::{
    collections::HashSet,
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    data_root: Option<PathBuf>,
    bind: Option<String>,
    assets: Option<PathBuf>,
    upload_idle_seconds: Option<i64>,
    trusted_proxy_peers: Option<Vec<IpAddr>>,
}

pub struct ProductionConfig {
    pub data_root: PathBuf,
    pub bind: SocketAddr,
    pub assets: PathBuf,
    pub upload_idle_seconds: i64,
    pub trusted_proxy_peers: HashSet<IpAddr>,
}

fn override_value(key: &str, current: Option<String>) -> Option<String> {
    env::var(key).ok().or(current)
}

pub fn production(path: Option<&Path>) -> Result<ProductionConfig, Box<dyn std::error::Error>> {
    const ALLOWED: &[&str] = &[
        "APP_DEPOT_DATA_ROOT",
        "APP_DEPOT_BIND",
        "APP_DEPOT_ASSETS",
        "APP_DEPOT_UPLOAD_IDLE_SECONDS",
        "APP_DEPOT_TRUSTED_PROXY_PEERS",
    ];
    for (name, _) in env::vars_os() {
        if let Some(name) = name.to_str() {
            if name.starts_with("APP_DEPOT_") && !ALLOWED.contains(&name) {
                return Err(format!("unknown App Depot setting: {name}").into());
            }
        }
    }
    let file: FileConfig = if let Some(path) = path {
        let metadata = path.symlink_metadata()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("configuration must be a regular file".into());
        }
        serde_json::from_slice(&fs::read(path)?)?
    } else {
        FileConfig::default()
    };
    let data_root = PathBuf::from(
        override_value(
            "APP_DEPOT_DATA_ROOT",
            file.data_root.map(|p| p.to_string_lossy().into_owned()),
        )
        .ok_or("Production data_root is required")?,
    );
    if !data_root.is_absolute() {
        return Err("Production data_root must be absolute".into());
    }
    let bind: SocketAddr = override_value("APP_DEPOT_BIND", file.bind)
        .unwrap_or_else(|| "0.0.0.0:8787".into())
        .parse()?;
    let assets = PathBuf::from(
        override_value(
            "APP_DEPOT_ASSETS",
            file.assets.map(|p| p.to_string_lossy().into_owned()),
        )
        .unwrap_or_else(|| "/app/web".into()),
    );
    let upload_idle_seconds: i64 = override_value(
        "APP_DEPOT_UPLOAD_IDLE_SECONDS",
        file.upload_idle_seconds.map(|v| v.to_string()),
    )
    .unwrap_or_else(|| "86400".into())
    .parse()?;
    if upload_idle_seconds < 1 {
        return Err("upload_idle_seconds must be positive".into());
    }
    let trusted_proxy_peers = if let Ok(value) = env::var("APP_DEPOT_TRUSTED_PROXY_PEERS") {
        if value.is_empty() {
            HashSet::new()
        } else {
            value
                .split(',')
                .map(str::parse)
                .collect::<Result<HashSet<IpAddr>, _>>()?
        }
    } else {
        file.trusted_proxy_peers
            .unwrap_or_default()
            .into_iter()
            .collect()
    };
    Ok(ProductionConfig {
        data_root,
        bind,
        assets,
        upload_idle_seconds,
        trusted_proxy_peers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, r#"{"data_root":"/tmp/example","unexpected":true}"#).unwrap();
        assert!(production(Some(&config)).is_err());
        fs::write(&config, r#"{"data_root":"/tmp/example"}"#).unwrap();
        assert!(production(Some(&config)).is_ok());
    }
}
