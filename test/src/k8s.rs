//! The apiserver, as the suites use it: this Job's own pod (for the image and
//! the node), and workload pods in the run's namespace, every one labelled
//! `storm.io/test-run=<run id>`. The runner's Role covers only that
//! namespace, so nothing cluster-scoped is read or written.
//!
//! Authenticated with the Job's ServiceAccount token and verified against the
//! mounted `ca.crt`. Without one (a hand run) it accepts the apiserver's
//! certificate unverified.

use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{json, Value};

use crate::env::Env;
use crate::workload::Load;

/// The label every workload pod carries besides the run's.
pub const WORKLOAD_LABEL: &str = "cadvisor-test/workload";

#[derive(Clone)]
pub struct Api {
    base: String,
    http: reqwest::Client,
    token: Option<String>,
    pub ns: String,
    pub run: String,
}

/// Where this Job's pod runs, and what it runs.
#[derive(Debug, Clone)]
pub struct Me {
    pub image: String,
    pub node_name: Option<String>,
    pub host_ip: Option<String>,
}

impl Api {
    pub fn new(env: &Env) -> Result<Api, String> {
        if env.api.is_empty() {
            return Err("STORM_API is not set".into());
        }
        let mut b = reqwest::Client::builder().timeout(Duration::from_secs(20));
        b = match &env.ca {
            Some(pem) => b.add_root_certificate(reqwest::Certificate::from_pem(pem).map_err(|e| format!("ca.crt: {e}"))?),
            None => b.danger_accept_invalid_certs(true),
        };
        Ok(Api {
            base: env.api.trim_end_matches('/').to_string(),
            http: b.build().map_err(|e| format!("http client: {e}"))?,
            token: env.token.clone(),
            ns: env.namespace.clone(),
            run: env.run_id.clone(),
        })
    }

    async fn call(&self, m: Method, path: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
        let mut rq = self.http.request(m.clone(), format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            rq = rq.bearer_auth(t);
        }
        if let Some(b) = body {
            rq = rq.json(b);
        }
        let resp = rq.send().await.map_err(|e| format!("{m} {path}: {e}"))?;
        let st = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Ok((st, serde_json::from_str(&text).unwrap_or(Value::String(text))))
    }

    fn pods(&self) -> String {
        format!("/api/v1/namespaces/{}/pods", self.ns)
    }

    pub async fn pod(&self, name: &str) -> Result<Option<Value>, String> {
        match self.call(Method::GET, &format!("{}/{name}", self.pods()), None).await? {
            (404, _) => Ok(None),
            (st, v) if (200..300).contains(&st) => Ok(Some(v)),
            (st, v) => Err(format!("GET pod {name}: {st} {}", msg(&v))),
        }
    }

    /// This Job's pod: by `HOSTNAME`, else the one pod of the run that is not
    /// a workload.
    pub async fn me(&self, hostname: &str) -> Result<Me, String> {
        let pod = match self.pod(hostname).await? {
            Some(p) => p,
            None => {
                let v = self.list().await?;
                let mine: Vec<&Value> = v.iter().filter(|p| p["metadata"]["labels"][WORKLOAD_LABEL].is_null()).collect();
                match mine.as_slice() {
                    [p] => (*p).clone(),
                    _ => return Err(format!("no pod named {hostname:?}, and {} candidate pods in {}", mine.len(), self.ns)),
                }
            }
        };
        let image = pod["spec"]["containers"][0]["image"].as_str().ok_or("this pod names no image")?.to_string();
        Ok(Me {
            image,
            node_name: pod["spec"]["nodeName"].as_str().map(str::to_string),
            host_ip: pod["status"]["hostIP"].as_str().map(str::to_string),
        })
    }

    /// This run's pods.
    pub async fn list(&self) -> Result<Vec<Value>, String> {
        let sel = format!("storm.io%2Ftest-run%3D{}", self.run);
        match self.call(Method::GET, &format!("{}?labelSelector={sel}", self.pods()), None).await? {
            (st, v) if (200..300).contains(&st) => Ok(v["items"].as_array().cloned().unwrap_or_default()),
            (st, v) => Err(format!("list pods: {st} {}", msg(&v))),
        }
    }

    pub async fn create_pod(&self, pod: &Value) -> Result<(), String> {
        match self.call(Method::POST, &self.pods(), Some(pod)).await? {
            (st, _) if (200..300).contains(&st) => Ok(()),
            (st, v) => Err(format!("create pod {}: {st} {}", pod["metadata"]["name"], msg(&v))),
        }
    }

    /// Delete now; already gone is fine.
    pub async fn delete_pod(&self, name: &str) -> Result<(), String> {
        match self.call(Method::DELETE, &format!("{}/{name}?gracePeriodSeconds=0", self.pods()), None).await? {
            (st, _) if st == 404 || (200..300).contains(&st) => Ok(()),
            (st, v) => Err(format!("delete pod {name}: {st} {}", msg(&v))),
        }
    }

    /// Until the pod is Running. A pod that ends first is an error naming why.
    pub async fn running(&self, name: &str, deadline: Instant) -> Result<Instant, String> {
        let mut last = String::from("not listed");
        while Instant::now() < deadline {
            if let Some(p) = self.pod(name).await? {
                let phase = p["status"]["phase"].as_str().unwrap_or("").to_string();
                match phase.as_str() {
                    "Running" => return Ok(Instant::now()),
                    "Succeeded" | "Failed" => return Err(format!("pod {name} ended ({phase}) before it was seen running: {}", why(&p))),
                    _ => last = format!("{phase} {}", why(&p)),
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(format!("pod {name} not running in time; last: {last}"))
    }

    /// How a pod's container ended, once it has: `(reason, exit code)`.
    pub async fn terminated(&self, name: &str) -> Result<Option<(String, i64)>, String> {
        let Some(p) = self.pod(name).await? else { return Ok(None) };
        let t = &p["status"]["containerStatuses"][0]["state"]["terminated"];
        if t.is_null() {
            return Ok(None);
        }
        Ok(Some((t["reason"].as_str().unwrap_or("").to_string(), t["exitCode"].as_i64().unwrap_or(-1))))
    }

    /// Delete every pod of this run except the Job's own. Returns how many.
    pub async fn cleanup(&self) -> Result<usize, String> {
        let mut n = 0;
        for p in self.list().await? {
            if p["metadata"]["labels"][WORKLOAD_LABEL].is_null() {
                continue;
            }
            if let Some(name) = p["metadata"]["name"].as_str() {
                self.delete_pod(name).await?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Workload pods of this run still listed.
    pub async fn leftovers(&self) -> Result<usize, String> {
        Ok(self.list().await?.iter().filter(|p| !p["metadata"]["labels"][WORKLOAD_LABEL].is_null()).count())
    }

    /// A workload pod: this Job's image running `/test workload <marker> …`,
    /// on this Job's node, never restarted.
    pub fn workload_pod(&self, name: &str, me: &Me, marker: &str, load: &Load) -> Value {
        let mut container = json!({
            "name": "workload",
            "image": me.image,
            "imagePullPolicy": "IfNotPresent",
            "command": ["/test"],
            "args": load.args(marker),
        });
        if let Some(mib) = load.limit_mib {
            let q = format!("{mib}Mi");
            container["resources"] = json!({"limits": {"memory": q}, "requests": {"memory": q}});
        }
        let mut spec = json!({
            "restartPolicy": "Never",
            "terminationGracePeriodSeconds": 0,
            "automountServiceAccountToken": false,
            "containers": [container],
        });
        if let Some(n) = &me.node_name {
            spec["nodeName"] = json!(n);
        }
        json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": name,
                "namespace": self.ns,
                "labels": {"storm.io/test-run": self.run, WORKLOAD_LABEL: "true"},
            },
            "spec": spec,
        })
    }
}

fn msg(v: &Value) -> String {
    let m = v["message"].as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
    crate::cad::clip(&m)
}

/// A pod's state in a few words, for error details.
fn why(p: &Value) -> String {
    let c = &p["status"]["containerStatuses"][0]["state"];
    for k in ["waiting", "terminated"] {
        if let Some(r) = c[k]["reason"].as_str() {
            return format!("{k}: {r} {}", c[k]["message"].as_str().unwrap_or(""));
        }
    }
    p["status"]["reason"].as_str().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api() -> Api {
        Api { base: "https://x".into(), http: reqwest::Client::new(), token: None, ns: "test-cadvisor-short-r1".into(), run: "r1".into() }
    }

    #[test]
    fn workload_pods_run_this_image_on_this_node_labelled_for_the_run() {
        let me = Me { image: "test-cadvisor-short:abc".into(), node_name: Some("n1".into()), host_ip: None };
        let load = Load { cpu_millis: 250, mem_mib: 64, secs: 300, oom_after: None, limit_mib: Some(96) };
        let p = api().workload_pod("w-a", &me, "cadvt-r1-a", &load);
        assert_eq!(p["metadata"]["namespace"], "test-cadvisor-short-r1");
        assert_eq!(p["metadata"]["labels"]["storm.io/test-run"], "r1");
        assert_eq!(p["metadata"]["labels"][WORKLOAD_LABEL], "true");
        assert_eq!(p["spec"]["nodeName"], "n1");
        assert_eq!(p["spec"]["restartPolicy"], "Never");
        let c = &p["spec"]["containers"][0];
        assert_eq!(c["image"], "test-cadvisor-short:abc");
        assert_eq!(c["command"][0], "/test");
        assert_eq!(c["args"][0], "workload");
        assert_eq!(c["args"][1], "cadvt-r1-a");
        assert_eq!(c["resources"]["limits"]["memory"], "96Mi");
    }
}
