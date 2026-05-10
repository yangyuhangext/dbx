use serde::{Deserialize, Serialize};
use std::fs::Metadata;
use std::path::PathBuf;

#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDiscovery {
    pub port: u16,
    pub token: String,
}

pub fn app_data_dir() -> PathBuf {
    if let Ok(path) = std::env::var("DBX_APP_DATA_DIR") {
        return PathBuf::from(path);
    }

    let home = std::env::var(if cfg!(windows) { "APPDATA" } else { "HOME" }).unwrap_or_else(|_| ".".to_string());

    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/com.dbx.app")
    } else if cfg!(windows) {
        PathBuf::from(home).join("com.dbx.app")
    } else {
        PathBuf::from(home).join(".config/com.dbx.app")
    }
}

pub fn load_runtime() -> Option<RuntimeDiscovery> {
    let path = app_data_dir().join("agent-runtime.json");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !is_secure_runtime_file(&metadata) {
        return None;
    }
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

pub async fn get_json(path: &str) -> Result<serde_json::Value, String> {
    let runtime = load_runtime().ok_or_else(|| "runtime unavailable".to_string())?;
    let url = runtime_url(&runtime, path, &[])?;

    let response =
        reqwest::Client::new().get(url).bearer_auth(runtime.token).send().await.map_err(|err| err.to_string())?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("runtime request failed with status {status}"));
    }

    response.json().await.map_err(|err| err.to_string())
}

pub async fn get_json_with_query(path: &str, query: &[(&str, String)]) -> Result<serde_json::Value, String> {
    let runtime = load_runtime().ok_or_else(|| "runtime unavailable".to_string())?;
    let url = runtime_url(&runtime, path, query)?;

    let response =
        reqwest::Client::new().get(url).bearer_auth(runtime.token).send().await.map_err(|err| err.to_string())?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("runtime request failed with status {status}"));
    }

    response.json().await.map_err(|err| err.to_string())
}

pub async fn post_json(path: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let runtime = load_runtime().ok_or_else(|| "runtime unavailable".to_string())?;
    let url = runtime_url(&runtime, path, &[])?;

    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(runtime.token)
        .json(&body)
        .send()
        .await
        .map_err(|err| err.to_string())?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("runtime request failed with status {status}"));
    }

    response.json().await.map_err(|err| err.to_string())
}

fn runtime_url(runtime: &RuntimeDiscovery, path: &str, query: &[(&str, String)]) -> Result<reqwest::Url, String> {
    if path.contains('\r') || path.contains('\n') || path.contains("://") {
        return Err("invalid runtime path".to_string());
    }

    let mut url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/", runtime.port)).map_err(|err| err.to_string())?;
    url.set_path(path.trim_start_matches('/'));
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    Ok(url)
}

fn is_secure_runtime_file(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    runtime_file_owner_and_mode_are_secure(metadata)
}

#[cfg(unix)]
fn runtime_file_owner_and_mode_are_secure(metadata: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    let owner_only = metadata.mode() & 0o077 == 0;
    let owned_by_effective_user = metadata.uid() == unsafe { libc::geteuid() };
    owner_only && owned_by_effective_user
}

#[cfg(not(unix))]
fn runtime_file_owner_and_mode_are_secure(_metadata: &Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn set_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(mode);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn load_runtime_rejects_symlink_discovery_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        let link = dir.path().join("agent-runtime.json");
        std::fs::write(&target, r#"{"port":4321,"token":"secret"}"#).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&link, r#"{"port":4321,"token":"secret"}"#).unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());

        assert!(load_runtime().is_none());

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn load_runtime_rejects_group_or_world_accessible_discovery_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let discovery = dir.path().join("agent-runtime.json");
        std::fs::write(&discovery, r#"{"port":4321,"token":"secret"}"#).unwrap();
        set_mode(&discovery, 0o644);
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());

        assert!(load_runtime().is_none());

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn load_runtime_accepts_owner_only_discovery_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let discovery = dir.path().join("agent-runtime.json");
        std::fs::write(&discovery, r#"{"port":4321,"token":"secret"}"#).unwrap();
        set_mode(&discovery, 0o600);
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());

        let runtime = load_runtime().expect("secure runtime discovery should load");
        assert_eq!(runtime.port, 4321);
        assert_eq!(runtime.token, "secret");

        std::env::remove_var("DBX_APP_DATA_DIR");
    }
}
