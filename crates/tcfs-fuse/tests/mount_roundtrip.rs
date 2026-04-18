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
        // Real FUSE teardown can briefly report EBUSY while the kernel finishes
        // releasing the mount. Give fusermount3 a short retry window instead of
        // failing the remount coverage on the first transient race.
        for attempt in 0..20 {
            if !is_mounted(&mountpoint)? {
                return Ok::<(), anyhow::Error>(());
            }

            let output = Command::new("fusermount3")
                .arg("-u")
                .arg(&mountpoint)
                .output()
                .with_context(|| format!("running fusermount3 -u {}", mountpoint.display()))?;

            if output.status.success() || !is_mounted(&mountpoint)? {
                return Ok::<(), anyhow::Error>(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("Device or resource busy") && attempt < 19 {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }

            anyhow::bail!(
                "fusermount3 -u {} failed: {}{}{}",
                mountpoint.display(),
                output.status,
                if stderr.trim().is_empty() {
                    ""
                } else {
                    " stderr: "
                },
                stderr.trim()
            );
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

async fn rename_path(from: PathBuf, to: PathBuf) -> Result<()> {
    let from_display = from.clone();
    let to_display = to.clone();
    tokio::task::spawn_blocking(move || std::fs::rename(&from, &to))
        .await
        .context("joining rename task")?
        .with_context(|| {
            format!(
                "renaming {} -> {}",
                from_display.display(),
                to_display.display()
            )
        })?;
    Ok(())
}

async fn remove_file(path: PathBuf) -> Result<()> {
    let display_path = path.clone();
    tokio::task::spawn_blocking(move || std::fs::remove_file(&path))
        .await
        .context("joining remove_file task")?
        .with_context(|| format!("removing file {}", display_path.display()))?;
    Ok(())
}

async fn remove_dir(path: PathBuf) -> Result<()> {
    let display_path = path.clone();
    tokio::task::spawn_blocking(move || std::fs::remove_dir(&path))
        .await
        .context("joining remove_dir task")?
        .with_context(|| format!("removing dir {}", display_path.display()))?;
    Ok(())
}

async fn create_dir(path: PathBuf) -> Result<()> {
    let display_path = path.clone();
    tokio::task::spawn_blocking(move || std::fs::create_dir(&path))
        .await
        .context("joining create_dir task")?
        .with_context(|| format!("creating dir {}", display_path.display()))?;
    Ok(())
}

async fn read_dir_names(path: PathBuf) -> Result<Vec<String>> {
    let display_path = path.clone();
    tokio::task::spawn_blocking(move || {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok::<Vec<String>, std::io::Error>(names)
    })
    .await
    .context("joining read_dir task")?
    .with_context(|| format!("reading directory {}", display_path.display()))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_lifecycle_updates_remote_state_across_rename_and_delete() -> Result<()> {
    if let Some(reason) = skip_reason() {
        eprintln!("skipping FUSE lifecycle test: {reason}");
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
    let prefix = "fuse-lifecycle";
    let dir_key = format!("{prefix}/index/docs/.tcfs_dir");
    let old_key = format!("{prefix}/index/docs/old.txt");
    let new_key = format!("{prefix}/index/docs/new.txt");

    let first_mount =
        spawn_mount(op.clone(), prefix.to_string(), mountpoint.clone(), cache_a).await;
    wait_for_mount_state(&mountpoint, true).await?;

    let docs_dir = mountpoint.join("docs");
    create_dir(docs_dir.clone()).await?;
    assert!(
        op.read(&dir_key).await.is_ok(),
        "expected remote directory marker after mkdir"
    );

    let old_file = docs_dir.join("old.txt");
    write_file(old_file.clone(), b"v1 through fuse").await?;
    assert!(
        op.read(&old_key).await.is_ok(),
        "expected old remote index entry after write"
    );

    let renamed_file = docs_dir.join("new.txt");
    rename_path(old_file, renamed_file.clone()).await?;
    assert!(
        op.read(&old_key).await.is_err(),
        "old remote index entry should be removed after rename"
    );
    assert!(
        op.read(&new_key).await.is_ok(),
        "expected new remote index entry after rename"
    );

    unmount_fuse(&mountpoint).await?;
    tokio::time::timeout(Duration::from_secs(10), first_mount)
        .await
        .context("timed out waiting for first lifecycle mount task to finish")?
        .context("joining first lifecycle mount task")?
        .context("first lifecycle mount failed")?;
    wait_for_mount_state(&mountpoint, false).await?;

    let second_mount =
        spawn_mount(op.clone(), prefix.to_string(), mountpoint.clone(), cache_b).await;
    wait_for_mount_state(&mountpoint, true).await?;

    assert_eq!(read_file(renamed_file.clone()).await?, b"v1 through fuse");
    assert_eq!(
        read_dir_names(docs_dir.clone()).await?,
        vec!["new.txt".to_string()]
    );

    remove_file(renamed_file).await?;
    remove_dir(docs_dir.clone()).await?;

    let root_names = read_dir_names(mountpoint.clone()).await?;
    assert!(
        !root_names.iter().any(|name| name == "docs"),
        "docs directory should be gone after unlink+rmdir, got {root_names:?}"
    );

    unmount_fuse(&mountpoint).await?;
    tokio::time::timeout(Duration::from_secs(10), second_mount)
        .await
        .context("timed out waiting for second lifecycle mount task to finish")?
        .context("joining second lifecycle mount task")?
        .context("second lifecycle mount failed")?;
    wait_for_mount_state(&mountpoint, false).await?;

    assert!(
        op.read(&new_key).await.is_err(),
        "renamed remote index entry should be removed after unlink"
    );
    assert!(
        op.read(&dir_key).await.is_err(),
        "directory marker should be removed after rmdir"
    );

    Ok(())
}
