use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use opendal::services::Memory;
use opendal::Operator;
use tcfs_fuse::{mount, MountConfig};
use tempfile::TempDir;

fn memory_operator() -> Operator {
    Operator::new(Memory::default()).unwrap().finish()
}

fn skip_reason() -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        return Some("linux-only test");
    }
    if !Path::new("/dev/fuse").exists() {
        return Some("missing /dev/fuse");
    }
    match Command::new("fusermount3").arg("--help").output() {
        Ok(_) => None,
        Err(_) => Some("fusermount3 not available"),
    }
}

fn is_mounted(mountpoint: &Path) -> Result<bool> {
    let mountinfo =
        std::fs::read_to_string("/proc/self/mountinfo").context("reading /proc/self/mountinfo")?;
    let mountpoint = mountpoint.to_string_lossy();
    Ok(mountinfo.lines().any(|line| {
        line.split_whitespace()
            .nth(4)
            .map(|field| field == mountpoint)
            .unwrap_or(false)
    }))
}

async fn wait_for_mount_state(mountpoint: &Path, mounted: bool) -> Result<()> {
    for _ in 0..100 {
        if is_mounted(mountpoint)? == mounted {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!(
        "mountpoint {} did not reach mounted={} in time",
        mountpoint.display(),
        mounted
    );
}

async fn unmount_fuse(mountpoint: &Path) -> Result<()> {
    let mountpoint = mountpoint.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let status = Command::new("fusermount3")
            .arg("-u")
            .arg(&mountpoint)
            .status()
            .with_context(|| format!("running fusermount3 -u {}", mountpoint.display()))?;
        if !status.success() {
            anyhow::bail!("fusermount3 -u {} failed: {status}", mountpoint.display());
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("joining unmount task")??;
    Ok(())
}

async fn spawn_mount(
    op: Operator,
    prefix: String,
    mountpoint: PathBuf,
    cache_dir: PathBuf,
) -> tokio::task::JoinHandle<std::io::Result<()>> {
    tokio::spawn(async move {
        mount(
            MountConfig {
                op,
                prefix,
                mountpoint,
                cache_dir,
                cache_max_bytes: 16 * 1024 * 1024,
                negative_ttl_secs: 1,
                read_only: false,
                allow_other: false,
                on_flush: None,
                device_id: "test-device".into(),
                master_key: None,
            },
            None,
        )
        .await
    })
}

async fn write_file(path: PathBuf, bytes: &'static [u8]) -> Result<()> {
    let display_path = path.clone();
    tokio::task::spawn_blocking(move || std::fs::write(&path, bytes))
        .await
        .context("joining write task")?
        .with_context(|| format!("writing {}", display_path.display()))?;
    Ok(())
}

async fn read_file(path: PathBuf) -> Result<Vec<u8>> {
    let display_path = path.clone();
    tokio::task::spawn_blocking(move || std::fs::read(&path))
        .await
        .context("joining read task")?
        .with_context(|| format!("reading {}", display_path.display()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_roundtrip_persists_across_real_fuse_remount() -> Result<()> {
    if let Some(reason) = skip_reason() {
        eprintln!("skipping FUSE mount roundtrip test: {reason}");
        return Ok(());
    }

    let tmp = TempDir::new().unwrap();
    let mountpoint = tmp.path().join("mnt");
    let cache_a = tmp.path().join("cache-a");
    let cache_b = tmp.path().join("cache-b");
    std::fs::create_dir_all(&mountpoint).unwrap();
    std::fs::create_dir_all(&cache_a).unwrap();
    std::fs::create_dir_all(&cache_b).unwrap();

    let op = memory_operator();
    let prefix = "fuse-e2e";

    let first_mount =
        spawn_mount(op.clone(), prefix.to_string(), mountpoint.clone(), cache_a).await;
    wait_for_mount_state(&mountpoint, true).await?;

    let mounted_file = mountpoint.join("notes.txt");
    write_file(mounted_file.clone(), b"hello through fuse").await?;
    assert_eq!(
        read_file(mounted_file.clone()).await?,
        b"hello through fuse"
    );

    unmount_fuse(&mountpoint).await?;
    tokio::time::timeout(Duration::from_secs(10), first_mount)
        .await
        .context("timed out waiting for first mount task to finish")?
        .context("joining first mount task")?
        .context("first mount failed")?;
    wait_for_mount_state(&mountpoint, false).await?;

    assert!(
        op.read(&format!("{prefix}/index/notes.txt")).await.is_ok(),
        "expected remote index entry after FUSE write"
    );

    let second_mount =
        spawn_mount(op.clone(), prefix.to_string(), mountpoint.clone(), cache_b).await;
    wait_for_mount_state(&mountpoint, true).await?;

    assert_eq!(read_file(mounted_file).await?, b"hello through fuse");

    unmount_fuse(&mountpoint).await?;
    tokio::time::timeout(Duration::from_secs(10), second_mount)
        .await
        .context("timed out waiting for second mount task to finish")?
        .context("joining second mount task")?
        .context("second mount failed")?;
    wait_for_mount_state(&mountpoint, false).await?;

    Ok(())
}
