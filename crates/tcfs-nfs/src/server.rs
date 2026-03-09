//! NFS loopback server: binds an NFSv3 server on localhost and optionally
//! mounts it at a given path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use opendal::Operator;
use tokio::process::Command;
use tracing::{info, warn};

use tcfs_vfs::TcfsVfs;

use crate::adapter::NfsAdapter;

/// Configuration for the NFS loopback mount.
pub struct NfsMountConfig {
    /// OpenDAL operator for SeaweedFS
    pub op: Operator,
    /// Remote prefix (e.g. "mydata")
    pub prefix: String,
    /// Local mountpoint path
    pub mountpoint: PathBuf,
    /// Disk cache directory
    pub cache_dir: PathBuf,
    /// Max disk cache size in bytes
    pub cache_max_bytes: u64,
    /// Negative dentry cache TTL
    pub negative_ttl_secs: u64,
    /// Port to bind NFS server (0 = auto-assign)
    pub port: u16,
}

/// A running NFS loopback mount.
pub struct NfsMount {
    /// The port the NFS server is listening on
    pub port: u16,
    /// The mountpoint path
    pub mountpoint: PathBuf,
}

impl NfsMount {
    /// Unmount the NFS filesystem.
    pub async fn unmount(&self) -> Result<()> {
        let status = if cfg!(target_os = "macos") {
            Command::new("umount")
                .arg(&self.mountpoint)
                .status()
                .await?
        } else {
            Command::new("umount")
                .arg(&self.mountpoint)
                .status()
                .await?
        };

        if !status.success() {
            // Try lazy unmount on Linux
            if cfg!(target_os = "linux") {
                warn!("normal unmount failed, trying lazy unmount");
                Command::new("umount")
                    .arg("-l")
                    .arg(&self.mountpoint)
                    .status()
                    .await?;
            }
        }

        Ok(())
    }
}

/// Start the NFS loopback server and mount it.
///
/// This function:
/// 1. Creates a `TcfsVfs` from the config
/// 2. Wraps it in an `NfsAdapter`
/// 3. Binds an NFSv3 server on localhost
/// 4. Mounts the NFS export at the given mountpoint
/// 5. Returns a handle that blocks until unmounted
pub async fn serve_and_mount(cfg: NfsMountConfig) -> Result<()> {
    let vfs = Arc::new(TcfsVfs::new(
        cfg.op,
        cfg.prefix,
        cfg.cache_dir,
        cfg.cache_max_bytes,
        Duration::from_secs(cfg.negative_ttl_secs),
    ));

    let adapter = NfsAdapter::new(vfs);
    let bind_addr = format!("127.0.0.1:{}", cfg.port);

    info!(addr = %bind_addr, "starting NFS loopback server");

    let listener = NFSTcpListener::bind(&bind_addr, adapter)
        .await
        .context("failed to bind NFS server")?;

    let actual_port = listener.get_listen_port();
    info!(port = actual_port, "NFS server listening");

    // Mount in a background task
    let mountpoint = cfg.mountpoint.clone();
    tokio::spawn(async move {
        // Small delay to ensure server is ready
        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Err(e) = mount_nfs(actual_port, &mountpoint).await {
            warn!(error = %e, "NFS mount failed");
        } else {
            info!(mountpoint = %mountpoint.display(), port = actual_port, "NFS mounted successfully");
        }
    });

    // Block forever serving NFS requests
    listener.handle_forever().await?;

    Ok(())
}

/// Start the NFS server without mounting (useful for testing or manual mount).
pub async fn serve_only(cfg: NfsMountConfig) -> Result<u16> {
    let vfs = Arc::new(TcfsVfs::new(
        cfg.op,
        cfg.prefix,
        cfg.cache_dir,
        cfg.cache_max_bytes,
        Duration::from_secs(cfg.negative_ttl_secs),
    ));

    let adapter = NfsAdapter::new(vfs);
    let bind_addr = format!("127.0.0.1:{}", cfg.port);

    let listener = NFSTcpListener::bind(&bind_addr, adapter)
        .await
        .context("failed to bind NFS server")?;

    let port = listener.get_listen_port();
    info!(port = port, "NFS server listening (no auto-mount)");

    tokio::spawn(async move {
        if let Err(e) = listener.handle_forever().await {
            warn!(error = %e, "NFS server exited with error");
        }
    });

    Ok(port)
}

/// Execute the platform-specific NFS mount command.
async fn mount_nfs(port: u16, mountpoint: &Path) -> Result<()> {
    // Ensure mountpoint exists
    tokio::fs::create_dir_all(mountpoint)
        .await
        .context("creating mountpoint directory")?;

    let status = if cfg!(target_os = "macos") {
        // macOS: mount_nfs with resvport for localhost
        Command::new("mount_nfs")
            .arg("-o")
            .arg(format!(
                "tcp,vers=3,resvport,locallocks,port={}",
                port
            ))
            .arg(format!("127.0.0.1:/"))
            .arg(mountpoint)
            .status()
            .await
            .context("executing mount_nfs")?
    } else {
        // Linux: mount -t nfs
        Command::new("mount")
            .arg("-t")
            .arg("nfs")
            .arg("-o")
            .arg(format!(
                "tcp,vers=3,noacl,nolock,port={}",
                port
            ))
            .arg(format!("127.0.0.1:/"))
            .arg(mountpoint)
            .status()
            .await
            .context("executing mount -t nfs")?
    };

    if !status.success() {
        anyhow::bail!(
            "mount command failed with exit code: {:?}",
            status.code()
        );
    }

    Ok(())
}

/// Check if a path is an active NFS mount.
pub async fn is_mounted(mountpoint: &Path) -> bool {
    let output = Command::new("mount")
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mp_str = mountpoint.to_string_lossy();
            stdout.lines().any(|line| line.contains(&*mp_str) && line.contains("nfs"))
        }
        Err(_) => false,
    }
}
