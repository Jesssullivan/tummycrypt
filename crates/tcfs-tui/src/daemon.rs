use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;
use tracing::{debug, warn};

use tcfs_core::proto::{
    tcfs_daemon_client::TcfsDaemonClient, CredentialStatusResponse, Empty, StatusRequest,
    StatusResponse,
};

pub enum DaemonUpdate {
    Status(StatusResponse),
    Creds(CredentialStatusResponse),
    Disconnected(String),
}

async fn connect(socket_path: &Path) -> Result<TcfsDaemonClient<Channel>> {
    let path = socket_path.to_path_buf();
    let channel = Endpoint::from_static("http://[::]:0")
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(&path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;
    Ok(TcfsDaemonClient::new(channel))
}

pub async fn poll_daemon(socket_path: PathBuf, tx: mpsc::Sender<DaemonUpdate>) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(10);

    loop {
        debug!(socket = %socket_path.display(), "connecting to daemon");

        match connect(&socket_path).await {
            Ok(mut client) => {
                backoff = Duration::from_secs(1);
                debug!("connected to daemon");

                loop {
                    // Poll status
                    match client.status(StatusRequest {}).await {
                        Ok(resp) => {
                            if tx
                                .send(DaemonUpdate::Status(resp.into_inner()))
                                .await
                                .is_err()
                            {
                                return; // receiver dropped
                            }
                        }
                        Err(e) => {
                            let _ = tx
                                .send(DaemonUpdate::Disconnected(format!("status RPC: {e}")))
                                .await;
                            break;
                        }
                    }

                    // Poll credential status
                    match client.credential_status(Empty {}).await {
                        Ok(resp) => {
                            if tx
                                .send(DaemonUpdate::Creds(resp.into_inner()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            warn!("credential_status RPC failed: {e}");
                        }
                    }

                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            Err(e) => {
                let _ = tx
                    .send(DaemonUpdate::Disconnected(format!(
                        "connect {}: {e}",
                        socket_path.display()
                    )))
                    .await;
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}
