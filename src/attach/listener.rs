//! Accept loop and the sibling `host-<name>.attach.sock` socket (plan Q1: the JSON-RPC listener
//! stays byte-identical; attach has its own 0600 socket with the same peer-uid check).

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;

use super::conn::{serve_connection, ConnConfig, HostInfo};
use super::handshake::{HOST_PROTO_MAX, HOST_PROTO_MIN};
use super::msg::HostMsg;
use super::registry::SharedRegistry;

/// `sockaddr_un.sun_path` budget, same conservative value as the JSON-RPC socket.
const MAX_SOCKET_PATH_BYTES: usize = 103;
/// Concurrent "server full" refusals in flight; beyond this surplus connections are just closed.
const MAX_REFUSE_TASKS: usize = 16;
const REFUSE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long shutdown waits for connections to flush `Bye`.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// `<runtime_dir>/host-<name>.attach.sock`; the name is validated like daemon names.
pub fn attach_socket_path_in(runtime_dir: &Path, name: &str) -> Result<PathBuf, String> {
    crate::secure_fs::validate_safe_name(name)?;
    let path = runtime_dir.join(format!("host-{name}.attach.sock"));
    if path.as_os_str().len() >= MAX_SOCKET_PATH_BYTES {
        return Err(format!(
            "attach socket path '{}' exceeds the {MAX_SOCKET_PATH_BYTES}-byte Unix socket limit; use a shorter daemon name or BILTERM_RUNTIME_DIR",
            path.display()
        ));
    }
    Ok(path)
}

/// Source of connections. Implemented for `UnixListener` (uid-checked) and by in-memory fakes in tests.
pub trait AttachAcceptor {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    fn accept(&self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

#[cfg(unix)]
impl AttachAcceptor for tokio::net::UnixListener {
    type Stream = tokio::net::UnixStream;
    async fn accept(&self) -> io::Result<Self::Stream> {
        let (stream, _addr) = tokio::net::UnixListener::accept(self).await?;
        let peer = stream.peer_cred()?;
        crate::secure_fs::check_peer_uid(peer.uid(), crate::secure_fs::current_euid())?;
        Ok(stream)
    }
}

/// Removes the socket file when dropped.
#[cfg(unix)]
pub struct SocketFileGuard(PathBuf);

#[cfg(unix)]
impl Drop for SocketFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Binds `path` as a 0600 Unix socket. The caller holds the daemon lock, so an existing socket
/// file is a previous crash's leftover and is replaced; a non-socket file is never touched.
#[cfg(unix)]
pub fn bind_attach_socket(path: &Path) -> io::Result<(tokio::net::UnixListener, SocketFileGuard)> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        crate::secure_fs::create_private_dir_all(parent)?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("refusing to overwrite non-socket file at {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    let guard = SocketFileGuard(path.to_path_buf());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, guard))
}

/// Accepts connections until `shutdown` flips, then lets live connections send `Bye`.
pub async fn serve_attach<A: AttachAcceptor>(
    acceptor: A,
    registry: SharedRegistry,
    host: Arc<HostInfo>,
    mut shutdown: watch::Receiver<bool>,
    cfg: ConnConfig,
) {
    let mut conns: JoinSet<()> = JoinSet::new();
    let refuse_permits = Arc::new(Semaphore::new(MAX_REFUSE_TASKS));
    loop {
        while conns.try_join_next().is_some() {}
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            accepted = acceptor.accept() => match accepted {
                Ok(stream) => match registry.reserve() {
                    Ok(slot) => {
                        let (host, rx) = (host.clone(), shutdown.clone());
                        conns.spawn(async move {
                            let end = serve_connection(stream, slot, host, rx, cfg).await;
                            tracing::debug!(?end, "attach connection closed");
                        });
                    }
                    Err(_) => refuse_full(&mut conns, &refuse_permits, stream),
                },
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    tracing::warn!(error = %e, "attach connection rejected");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "attach accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async { while conns.join_next().await.is_some() {} }).await;
    conns.abort_all();
}

fn refuse_full<S>(conns: &mut JoinSet<()>, permits: &Arc<Semaphore>, mut stream: S)
where
    S: AsyncWrite + Unpin + Send + 'static,
{
    let Ok(permit) = permits.clone().try_acquire_owned() else { return };
    conns.spawn(async move {
        let msg = HostMsg::Refuse {
            reason: format!("host is full ({} attach clients)", super::limits::MAX_ATTACH_CLIENTS),
            host_proto_min: HOST_PROTO_MIN,
            host_proto_max: HOST_PROTO_MAX,
        };
        if let Ok(bytes) = msg.encode() {
            let _ = tokio::time::timeout(REFUSE_WRITE_TIMEOUT, stream.write_all(&bytes)).await;
        }
        let _ = stream.shutdown().await;
        drop(permit);
    });
}
