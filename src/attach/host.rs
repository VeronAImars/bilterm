//! Wiring for `bilterm --headless --host-ptys --attach`: binds the sibling attach socket, shares
//! the client registry with the control plane (`attach.clients`) and runs the accept loop.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::conn::{ConnConfig, HostInfo};
use super::limits::IN_QUEUE_FRAMES;
use super::listener::{attach_socket_path_in, bind_attach_socket, serve_attach};
use super::registry::SharedRegistry;
use crate::api::AutomationControlPlane;
use crate::error::{BiltermError, Result};

/// Running attach listener. Dropping it (or calling [`Self::stop`]) tells clients `Bye{host_shutdown}`.
pub struct AttachRuntime {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    pub socket_path: std::path::PathBuf,
}

impl AttachRuntime {
    /// Signals shutdown and waits (bounded) for the accept loop to finish.
    pub async fn stop(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(t) = self.task.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), t).await;
        }
    }
}

impl Drop for AttachRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Starts the attach listener for host `name`. The plane must be a session host.
#[cfg(unix)]
pub fn start_attach(runtime_dir: &Path, name: &str, plane: &mut AutomationControlPlane) -> Result<AttachRuntime> {
    if !plane.is_host() {
        return Err(BiltermError::Config("--attach requires --host-ptys".into()));
    }
    let socket_path = attach_socket_path_in(runtime_dir, name).map_err(BiltermError::Config)?;
    let (listener, guard) = bind_attach_socket(&socket_path)?;
    let registry = SharedRegistry::default();
    plane.wm.set_attach_registry(registry.clone());
    let (req_tx, req_rx) = tokio::sync::mpsc::channel(IN_QUEUE_FRAMES);
    plane.set_attach_requests(req_rx);
    let host = Arc::new(HostInfo {
        name: name.to_string(),
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        caps: Vec::new(),
        requests: Some(req_tx),
    });
    let (tx, rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let _guard = guard;
        serve_attach(listener, registry, host, rx, ConnConfig::default()).await;
    });
    Ok(AttachRuntime { shutdown: tx, task: Some(task), socket_path })
}

#[cfg(not(unix))]
pub fn start_attach(_: &Path, _: &str, _: &mut AutomationControlPlane) -> Result<AttachRuntime> {
    Err(BiltermError::Config("--attach is Unix-only for now (plan Q6)".into()))
}
