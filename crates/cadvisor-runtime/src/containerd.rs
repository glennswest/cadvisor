//! containerd gRPC client (containers.v1 + tasks.v1).

use containerd_client::services::v1::containers_client::ContainersClient;
use containerd_client::services::v1::tasks_client::TasksClient;
use containerd_client::services::v1::{GetContainerRequest, GetRequest};
// `with_namespace!` expands to `Request::new(..)`; the import must be in scope.
use containerd_client::tonic::Request;
use containerd_client::tonic::transport::Channel;
use containerd_client::with_namespace;

use crate::{ContainerMeta, RuntimeError};

/// The label CRI-containerd puts on pod sandbox (pause) containers.
const SANDBOX_KIND_LABEL: &str = "io.cri-containerd.kind";

pub struct ContainerdClient {
    channel: Channel,
    namespace: String,
}

impl ContainerdClient {
    pub async fn connect(socket: &str, namespace: &str) -> Result<Self, RuntimeError> {
        let channel = containerd_client::connect(socket)
            .await
            .map_err(|e| RuntimeError::Unavailable(e.to_string()))?;
        Ok(ContainerdClient { channel, namespace: namespace.to_string() })
    }

    pub async fn inspect(&self, id: &str) -> Result<ContainerMeta, RuntimeError> {
        let ns = self.namespace.as_str();
        let mut containers = ContainersClient::new(self.channel.clone());
        let req = GetContainerRequest { id: id.to_string() };
        let resp = containers
            .get(with_namespace!(req, ns))
            .await
            .map_err(|s| match s.code() {
                containerd_client::tonic::Code::NotFound => RuntimeError::NotFound,
                _ => RuntimeError::Other(s.to_string()),
            })?
            .into_inner();
        let container = resp.container.ok_or(RuntimeError::NotFound)?;

        // Init PID via tasks.v1 (absent for created-but-not-started).
        let mut tasks = TasksClient::new(self.channel.clone());
        let treq = GetRequest { container_id: id.to_string(), exec_id: String::new() };
        let init_pid = tasks
            .get(with_namespace!(treq, ns))
            .await
            .ok()
            .and_then(|r| r.into_inner().process.map(|p| p.pid));

        let labels: std::collections::BTreeMap<String, String> =
            container.labels.into_iter().collect();
        // Upstream's containerd handler aliases by bare id only (verified
        // against real v0.49.2 output: name label = the 64-hex id).
        let aliases = vec![id.to_string()];
        let reports_network = labels.get(SANDBOX_KIND_LABEL).map(String::as_str) == Some("sandbox");

        Ok(ContainerMeta {
            id: id.to_string(),
            namespace: "containerd".to_string(),
            aliases,
            image: container.image,
            labels,
            init_pid,
            reports_network,
            // containerd provides no fs-usage stats (matches upstream).
            rootfs_diff: None,
        })
    }
}
