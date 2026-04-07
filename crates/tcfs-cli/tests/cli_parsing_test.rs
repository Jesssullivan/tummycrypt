//! CLI argument parsing tests for tcfs.
//!
//! Tests clap argument parsing without invoking real gRPC/network operations.
//! Uses clap's `try_parse_from` to validate command structure.

use clap::Parser;
use std::path::PathBuf;

// ── Replicate CLI structs for testing (binary crates can't be imported) ──

#[derive(Parser, Debug)]
#[command(name = "tcfs")]
struct Cli {
    #[arg(
        long,
        short = 'c',
        env = "TCFS_CONFIG",
        default_value = "/etc/tcfs/config.toml"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Status,
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    Push {
        local: PathBuf,
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },
    Pull {
        manifest: String,
        local: Option<PathBuf>,
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },
    #[command(name = "sync-status")]
    SyncStatus {
        path: Option<PathBuf>,
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },
    Mount {
        remote: String,
        mountpoint: PathBuf,
        #[arg(long)]
        read_only: bool,
        #[arg(long)]
        nfs: bool,
        #[arg(long, default_value = "0")]
        nfs_port: u16,
    },
    Unmount {
        mountpoint: PathBuf,
    },
    Unsync {
        path: PathBuf,
        #[arg(long)]
        force: bool,
    },
    Init {
        #[arg(long)]
        device_name: Option<String>,
        #[arg(long)]
        non_interactive: bool,
        #[arg(long, env = "TCFS_MASTER_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
}

#[derive(clap::Subcommand, Debug)]
enum ConfigAction {
    Show,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn parse_status() {
    let cli = Cli::try_parse_from(["tcfs", "status"]).expect("status should parse");
    assert!(matches!(cli.command, Commands::Status));
    // Default config path depends on TCFS_CONFIG env var; just verify it's non-empty
    assert!(!cli.config.as_os_str().is_empty());
}

#[test]
fn parse_status_with_custom_config() {
    let cli = Cli::try_parse_from(["tcfs", "-c", "/tmp/test.toml", "status"])
        .expect("custom config should parse");
    assert_eq!(cli.config, PathBuf::from("/tmp/test.toml"));
}

#[test]
fn parse_config_show() {
    let cli = Cli::try_parse_from(["tcfs", "config", "show"]).expect("config show should parse");
    assert!(matches!(
        cli.command,
        Commands::Config {
            action: ConfigAction::Show
        }
    ));
}

#[test]
fn parse_push_minimal() {
    let cli = Cli::try_parse_from(["tcfs", "push", "/path/to/file"]).expect("push should parse");
    if let Commands::Push {
        local,
        prefix,
        state,
    } = cli.command
    {
        assert_eq!(local, PathBuf::from("/path/to/file"));
        assert!(prefix.is_none());
        assert!(state.is_none());
    } else {
        panic!("expected Push");
    }
}

#[test]
fn parse_push_with_prefix() {
    let cli = Cli::try_parse_from(["tcfs", "push", "/src", "--prefix", "mydata"])
        .expect("push with prefix");
    if let Commands::Push { prefix, .. } = cli.command {
        assert_eq!(prefix, Some("mydata".to_string()));
    } else {
        panic!("expected Push");
    }
}

#[test]
fn parse_pull() {
    let cli = Cli::try_parse_from(["tcfs", "pull", "data/manifests/abc123", "/tmp/output"])
        .expect("pull should parse");
    if let Commands::Pull {
        manifest,
        local,
        prefix,
        ..
    } = cli.command
    {
        assert_eq!(manifest, "data/manifests/abc123");
        assert_eq!(local, Some(PathBuf::from("/tmp/output")));
        assert!(prefix.is_none());
    } else {
        panic!("expected Pull");
    }
}

#[test]
fn parse_sync_status() {
    let cli = Cli::try_parse_from(["tcfs", "sync-status"]).expect("sync-status should parse");
    if let Commands::SyncStatus { path, .. } = cli.command {
        assert!(path.is_none());
    } else {
        panic!("expected SyncStatus");
    }
}

#[test]
fn parse_sync_status_with_path() {
    let cli =
        Cli::try_parse_from(["tcfs", "sync-status", "/home/user/docs"]).expect("sync-status path");
    if let Commands::SyncStatus { path, .. } = cli.command {
        assert_eq!(path, Some(PathBuf::from("/home/user/docs")));
    } else {
        panic!("expected SyncStatus");
    }
}

#[test]
fn parse_mount() {
    let cli = Cli::try_parse_from([
        "tcfs",
        "mount",
        "seaweedfs://localhost:8333/bucket",
        "/mnt/tcfs",
        "--read-only",
    ])
    .expect("mount should parse");
    if let Commands::Mount {
        remote,
        mountpoint,
        read_only,
        nfs,
        nfs_port,
    } = cli.command
    {
        assert_eq!(remote, "seaweedfs://localhost:8333/bucket");
        assert_eq!(mountpoint, PathBuf::from("/mnt/tcfs"));
        assert!(read_only);
        assert!(!nfs);
        assert_eq!(nfs_port, 0);
    } else {
        panic!("expected Mount");
    }
}

#[test]
fn parse_mount_nfs() {
    let cli = Cli::try_parse_from([
        "tcfs",
        "mount",
        "seaweedfs://host/bucket",
        "/mnt",
        "--nfs",
        "--nfs-port",
        "2049",
    ])
    .expect("mount nfs");
    if let Commands::Mount { nfs, nfs_port, .. } = cli.command {
        assert!(nfs);
        assert_eq!(nfs_port, 2049);
    } else {
        panic!("expected Mount");
    }
}

#[test]
fn parse_unmount() {
    let cli = Cli::try_parse_from(["tcfs", "unmount", "/mnt/tcfs"]).expect("unmount");
    if let Commands::Unmount { mountpoint } = cli.command {
        assert_eq!(mountpoint, PathBuf::from("/mnt/tcfs"));
    } else {
        panic!("expected Unmount");
    }
}

#[test]
fn parse_unsync() {
    let cli = Cli::try_parse_from(["tcfs", "unsync", "/path/to/file.txt"]).expect("unsync");
    if let Commands::Unsync { path, force } = cli.command {
        assert_eq!(path, PathBuf::from("/path/to/file.txt"));
        assert!(!force);
    } else {
        panic!("expected Unsync");
    }
}

#[test]
fn parse_unsync_force() {
    let cli = Cli::try_parse_from(["tcfs", "unsync", "--force", "/file"]).expect("unsync force");
    if let Commands::Unsync { force, .. } = cli.command {
        assert!(force);
    } else {
        panic!("expected Unsync");
    }
}

#[test]
fn parse_init() {
    let cli = Cli::try_parse_from(["tcfs", "init", "--device-name", "neo", "--non-interactive"])
        .expect("init");
    if let Commands::Init {
        device_name,
        non_interactive,
        ..
    } = cli.command
    {
        assert_eq!(device_name, Some("neo".to_string()));
        assert!(non_interactive);
    } else {
        panic!("expected Init");
    }
}

#[test]
fn unknown_command_fails() {
    let result = Cli::try_parse_from(["tcfs", "bogus"]);
    assert!(result.is_err());
}

#[test]
fn missing_required_arg_fails() {
    let result = Cli::try_parse_from(["tcfs", "push"]);
    assert!(result.is_err(), "push without local path should fail");
}

#[test]
fn help_flag_works() {
    // --help causes clap to return an error with kind DisplayHelp
    let result = Cli::try_parse_from(["tcfs", "--help"]);
    assert!(result.is_err());
}
