//! CRI-O metadata client: HTTP/1 over the crio unix socket.

use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::UnixStream;

use crate::{ContainerMeta, RuntimeError};

const POD_NAME_LABEL: &str = "io.kubernetes.container.name";

#[derive(Debug, Clone)]
pub struct CrioClient {
    socket: String,
}

/// `GET /info` response (fields we use).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CrioInfo {
    #[serde(default)]
    pub storage_driver: String,
    #[serde(default)]
    pub storage_root: String,
    #[serde(default)]
    pub cgroup_driver: String,
}

/// CRI-O emits explicit `null` for empty maps/lists; treat null as default.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Option::unwrap_or_default)
}

/// `GET /containers/<id>` response (fields we use).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CrioContainerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub image: String,
    #[serde(default, deserialize_with = "null_default")]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default, deserialize_with = "null_default")]
    pub annotations: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub root: String,
    #[serde(default, deserialize_with = "null_default")]
    pub ip_address: String,
    #[serde(default, deserialize_with = "null_default")]
    pub ip_addresses: Vec<String>,
}

impl CrioClient {
    pub fn new(socket: &str) -> Self {
        CrioClient { socket: socket.to_string() }
    }

    pub fn available(&self) -> bool {
        std::path::Path::new(&self.socket).exists()
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, RuntimeError> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| RuntimeError::Unavailable(e.to_string()))?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| RuntimeError::Other(e.to_string()))?;
        tokio::spawn(conn);
        let req = hyper::Request::builder()
            .uri(path)
            .header(hyper::header::HOST, "crio")
            .body(Empty::<Bytes>::new())
            .map_err(|e| RuntimeError::Other(e.to_string()))?;
        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| RuntimeError::Other(e.to_string()))?;
        if resp.status() == hyper::StatusCode::NOT_FOUND {
            return Err(RuntimeError::NotFound);
        }
        if !resp.status().is_success() {
            return Err(RuntimeError::Other(format!("crio {path}: HTTP {}", resp.status())));
        }
        let body = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| RuntimeError::Other(e.to_string()))?
            .to_bytes();
        serde_json::from_slice(&body).map_err(|e| RuntimeError::Other(e.to_string()))
    }

    pub async fn info(&self) -> Result<CrioInfo, RuntimeError> {
        self.get_json("/info").await
    }

    pub async fn inspect(&self, id: &str) -> Result<ContainerMeta, RuntimeError> {
        let info: CrioContainerInfo = self.get_json(&format!("/containers/{id}")).await?;
        let reports_network = info.labels.get(POD_NAME_LABEL).map(String::as_str) == Some("POD");
        let mut aliases = Vec::new();
        if !info.name.is_empty() {
            aliases.push(info.name.clone());
        }
        aliases.push(id.to_string());
        let rootfs_diff = info
            .root
            .strip_suffix("/merged")
            .map(|base| format!("{base}/diff"))
            .filter(|p| std::path::Path::new(p).is_dir());
        Ok(ContainerMeta {
            id: id.to_string(),
            namespace: "crio".to_string(),
            aliases,
            image: info.image,
            labels: info.labels,
            init_pid: (info.pid > 0).then_some(info.pid),
            reports_network,
            rootfs_diff,
        })
    }
}
