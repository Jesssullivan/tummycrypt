//! Fleet E2E: Live NATS + SeaweedFS on CIVO K3s
//!
//! These tests connect to REAL infrastructure via Tailscale:
//! - SeaweedFS S3: seaweedfs-filer-ts (100.120.66.67:8333)
//! - NATS JetStream: nats-tcfs (100.71.19.127:4222)
//!
//! Gated by TCFS_E2E_LIVE=1. Skips automatically otherwise.
//! The canonical live acceptance lane is `neo-honey`.
//!
//! Run:
//!   TCFS_E2E_LIVE=1 \
//!   TCFS_S3_ENDPOINT=http://100.120.66.67:8333 \
//!   TCFS_S3_BUCKET=tcfs \
//!   AWS_ACCESS_KEY_ID=<from k8s secret seaweedfs-admin> \
//!   AWS_SECRET_ACCESS_KEY=<from k8s secret seaweedfs-admin> \
//!   TCFS_NATS_URL=nats://100.71.19.127:4222 \
//!   cargo test -p tcfs-e2e --test fleet_live -- --nocapture
//!
//! Or run the named smoke lane wrapper:
//!   just neo-honey-smoke

use std::time::Duration;

use futures::StreamExt;
use tcfs_e2e::write_test_file;
use tempfile::TempDir;
use tokio::time::{timeout, Instant};

use tcfs_sync::conflict::VectorClock;
use tcfs_sync::manifest::SyncManifest;
use tcfs_sync::nats::{NatsClient, StateEvent, StateEventMessage};

const NEO_DEVICE: &str = "neo";
const HONEY_DEVICE: &str = "honey";
const NEO_HONEY_PREFIX: &str = "neo-honey";

/// Check if live E2E is enabled via env var
fn live_enabled() -> bool {
    std::env::var("TCFS_E2E_LIVE").unwrap_or_default() == "1"
}

/// Get S3 endpoint from env or default to CIVO Tailscale IP
fn s3_endpoint() -> String {
    std::env::var("TCFS_S3_ENDPOINT").unwrap_or_else(|_| "http://100.120.66.67:8333".into())
}

fn broken_s3_endpoint() -> String {
    std::env::var("TCFS_S3_BROKEN_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:1".into())
}

fn s3_bucket() -> String {
    std::env::var("TCFS_S3_BUCKET").unwrap_or_else(|_| "tcfs".into())
}

fn nats_url() -> String {
    std::env::var("TCFS_NATS_URL").unwrap_or_else(|_| "nats://100.71.19.127:4222".into())
}

fn sample_state_event(
    device_id: &str,
    rel_path: &str,
    manifest_path: &str,
    timestamp: u64,
) -> StateEvent {
    let mut vclock = VectorClock::new();
    vclock.tick(device_id);
    StateEvent::FileSynced {
        device_id: device_id.into(),
        rel_path: rel_path.into(),
        blake3: format!("blake3-{timestamp}"),
        size: rel_path.len() as u64,
        vclock,
        manifest_path: manifest_path.into(),
        timestamp,
    }
}

fn matches_rel_path(event: &StateEvent, rel_path: &str) -> bool {
    match event {
        StateEvent::FileSynced {
            rel_path: event_rel_path,
            ..
        } => event_rel_path == rel_path,
        _ => false,
    }
}

async fn drain_until_idle<S>(stream: &mut S, idle_for: Duration)
where
    S: futures::Stream<Item = anyhow::Result<StateEventMessage>> + Unpin,
{
    loop {
        match timeout(idle_for, stream.next()).await {
            Ok(Some(Ok(msg))) => {
                msg.ack().await.expect("ack drained state event");
            }
            Ok(Some(Err(err))) => panic!("receiving drained state event: {err}"),
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

async fn recv_matching_event<S>(stream: &mut S, rel_path: &str, within: Duration) -> StateEvent
where
    S: futures::Stream<Item = anyhow::Result<StateEventMessage>> + Unpin,
{
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for state event with rel_path={rel_path}"
        );
        let msg = timeout(remaining, stream.next())
            .await
            .expect("timeout waiting for state event")
            .expect("state stream ended")
            .expect("receiving state event");
        let event = msg.event.clone();
        msg.ack().await.expect("ack state event");
        if matches_rel_path(&event, rel_path) {
            return event;
        }
    }
}

async fn assert_no_matching_event<S>(stream: &mut S, rel_path: &str, within: Duration)
where
    S: futures::Stream<Item = anyhow::Result<StateEventMessage>> + Unpin,
{
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match timeout(remaining, stream.next()).await {
            Ok(Some(Ok(msg))) => {
                let event = msg.event.clone();
                msg.ack().await.expect("ack state event");
                assert!(
                    !matches_rel_path(&event, rel_path),
                    "ephemeral consumer replayed old event for rel_path={rel_path}"
                );
            }
            Ok(Some(Err(err))) => panic!("receiving state event: {err}"),
            Ok(None) => return,
            Err(_) => return,
        }
    }
}

/// Build an opendal S3 operator from env credentials
fn live_operator() -> Option<opendal::Operator> {
    live_operator_for_endpoint(&s3_endpoint())
}

fn live_operator_for_endpoint(endpoint: &str) -> Option<opendal::Operator> {
    let access = std::env::var("AWS_ACCESS_KEY_ID").ok()?;
    let secret = std::env::var("AWS_SECRET_ACCESS_KEY").ok()?;
    let bucket = s3_bucket();

    let config = tcfs_storage::operator::StorageConfig {
        endpoint: endpoint.into(),
        region: "us-east-1".into(),
        bucket,
        access_key_id: access,
        secret_access_key: secret,
    };

    tcfs_storage::operator::build_operator(&config).ok()
}

async fn cleanup_upload_objects(
    op: &opendal::Operator,
    prefix: &str,
    upload: &tcfs_sync::engine::UploadResult,
) {
    let index_key = format!(
        "{}/index/{}",
        prefix,
        upload.path.file_name().unwrap().to_string_lossy()
    );

    if let Ok(manifest_bytes) = op.read(&upload.remote_path).await {
        if let Ok(manifest) = SyncManifest::from_bytes(&manifest_bytes.to_bytes()) {
            for chunk_hash in manifest.chunk_hashes() {
                let chunk_key = format!("{}/chunks/{}", prefix, chunk_hash);
                let _ = op.delete(&chunk_key).await;
            }
        }
    }

    let _ = op.delete(&upload.remote_path).await;
    let _ = op.delete(&index_key).await;
}

// ── SeaweedFS connectivity ───────────────────────────────────────────────

#[tokio::test]
async fn seaweedfs_health_check() {
    if !live_enabled() {
        eprintln!("SKIP: TCFS_E2E_LIVE not set");
        return;
    }

    let op = live_operator()
        .expect("S3 credentials required (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY)");
    let result = tcfs_storage::check_health(&op).await;
    assert!(result.is_ok(), "SeaweedFS health check failed: {result:?}");
}

// ── NATS connectivity ────────────────────────────────────────────────────

#[tokio::test]
async fn nats_connect_and_jetstream() {
    if !live_enabled() {
        eprintln!("SKIP: TCFS_E2E_LIVE not set");
        return;
    }

    let url = nats_url();
    let client = async_nats::connect(&url)
        .await
        .unwrap_or_else(|e| panic!("NATS connect to {url} failed: {e}"));

    // Verify JetStream is available
    let js = async_nats::jetstream::new(client.clone());
    // JetStream context creation succeeds if the server supports it
    // Verify by attempting to list streams
    let mut streams = js.streams();
    let mut stream_count = 0u32;
    while let Some(stream) = streams.next().await {
        match stream {
            Ok(s) => {
                eprintln!(
                    "  stream: {} ({} messages)",
                    s.config.name, s.state.messages
                );
                stream_count += 1;
            }
            Err(e) => eprintln!("  stream list error: {e}"),
        }
    }
    eprintln!("NATS JetStream: {stream_count} streams found");
}

// ── Push → Pull roundtrip via live SeaweedFS ─────────────────────────────

#[tokio::test]
async fn live_push_pull_roundtrip() {
    if !live_enabled() {
        eprintln!("SKIP: TCFS_E2E_LIVE not set");
        return;
    }

    let op = live_operator().expect("S3 credentials required");
    let tmp = TempDir::new().unwrap();

    // Use a unique prefix to avoid collisions with other tests/hosts
    let test_id = uuid::Uuid::new_v4().to_string();
    let prefix = format!("e2e-test/{}", &test_id[..8]);

    let content = format!("Fleet E2E test content — {test_id}").into_bytes();
    let src = write_test_file(tmp.path(), "fleet-test.txt", &content);
    let dst = tmp.path().join("pulled.txt");

    let state_path = tmp.path().join("state.db.json");
    let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();

    // Push
    let upload = tcfs_sync::engine::upload_file(&op, &src, &prefix, &mut state, None)
        .await
        .expect("live push to SeaweedFS");

    assert!(!upload.skipped);
    eprintln!(
        "Pushed: {} ({} bytes, {} chunks, manifest={})",
        src.display(),
        upload.bytes,
        upload.chunks,
        upload.remote_path
    );

    // Pull
    let download = tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, &prefix, None)
        .await
        .expect("live pull from SeaweedFS");

    assert_eq!(download.bytes, content.len() as u64);

    let pulled = std::fs::read(&dst).unwrap();
    assert_eq!(pulled, content, "roundtrip content mismatch");

    eprintln!("Roundtrip verified: {} bytes match", content.len());

    // Cleanup: delete the test objects
    let _ = op.delete(&upload.remote_path).await;
    let index_key = format!("{}/index/fleet-test.txt", prefix);
    let _ = op.delete(&index_key).await;
}

#[tokio::test]
async fn live_storage_outage_leaves_no_remote_index_and_recovers() {
    if !live_enabled() {
        eprintln!("SKIP: TCFS_E2E_LIVE not set");
        return;
    }

    let good_op = live_operator().expect("S3 credentials required");
    let bad_op = live_operator_for_endpoint(&broken_s3_endpoint())
        .expect("S3 credentials required for broken operator");
    let tmp = TempDir::new().unwrap();

    let test_id = uuid::Uuid::new_v4().to_string();
    let prefix = format!("e2e-outage/{}", &test_id[..8]);
    let rel_path = "offline.txt";
    let content = format!("storage outage recovery content {test_id}").into_bytes();
    let src = write_test_file(tmp.path(), rel_path, &content);
    let state_path = tmp.path().join("state.db.json");
    let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();

    let outage = tcfs_sync::engine::upload_file(&bad_op, &src, &prefix, &mut state, None)
        .await
        .expect_err("broken endpoint should fail upload");
    eprintln!("expected storage outage failure: {outage:#}");

    let remote_index = tcfs_sync::reconcile::list_remote_index(&good_op, &prefix)
        .await
        .expect("list remote index after failed upload");
    assert!(
        remote_index.is_empty(),
        "failed upload must not publish a remote index entry"
    );

    let remote_objects = good_op
        .list(&format!("{prefix}/"))
        .await
        .expect("list remote prefix after failed upload");
    assert!(
        remote_objects.is_empty(),
        "failed upload should not leave live objects under the test prefix"
    );

    let recovered = tcfs_sync::engine::upload_file(&good_op, &src, &prefix, &mut state, None)
        .await
        .expect("retry after storage recovery");
    assert!(!recovered.skipped, "recovery retry should upload content");

    let remote_index = tcfs_sync::reconcile::list_remote_index(&good_op, &prefix)
        .await
        .expect("list remote index after recovery");
    let visible = remote_index
        .get(rel_path)
        .expect("recovered upload should publish remote index");
    assert_eq!(visible.manifest_hash, recovered.hash);

    cleanup_upload_objects(&good_op, &prefix, &recovered).await;
}

// ── NATS event publish + subscribe ───────────────────────────────────────

#[tokio::test]
async fn live_nats_pubsub_roundtrip() {
    if !live_enabled() {
        eprintln!("SKIP: TCFS_E2E_LIVE not set");
        return;
    }

    let url = nats_url();
    let client = async_nats::connect(&url).await.expect("NATS connect");

    let test_id = uuid::Uuid::new_v4().to_string();
    let subject = format!("tcfs.e2e.test.{}", &test_id[..8]);

    // Subscribe first
    let mut sub = client.subscribe(subject.clone()).await.expect("subscribe");

    // Publish
    let payload = format!("e2e-test-{test_id}");
    client
        .publish(subject.clone(), payload.clone().into())
        .await
        .expect("publish");
    client.flush().await.expect("flush");

    // Receive with timeout
    let msg = tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .expect("timeout waiting for NATS message")
        .expect("no message received");

    assert_eq!(
        String::from_utf8_lossy(&msg.payload),
        payload,
        "NATS message payload mismatch"
    );

    eprintln!("NATS pubsub verified on subject: {subject}");
}

#[tokio::test]
async fn live_state_consumer_replay_and_ephemeral_new_only() {
    if !live_enabled() {
        eprintln!("SKIP: TCFS_E2E_LIVE not set");
        return;
    }

    let test_id = uuid::Uuid::new_v4().to_string();
    let publisher_id = format!("live-pub-{}", &test_id[..8]);
    let durable_id = format!("live-durable-{}", &test_id[..8]);
    let first_rel_path = format!("replay/{}/before-reconnect.txt", &test_id[..8]);
    let second_rel_path = format!("replay/{}/after-ephemeral.txt", &test_id[..8]);
    let first_manifest = format!("manifests/{}/first", &test_id[..8]);
    let second_manifest = format!("manifests/{}/second", &test_id[..8]);

    let publisher = NatsClient::connect(&nats_url(), false, None)
        .await
        .expect("connect publisher NATS client");
    publisher
        .ensure_streams()
        .await
        .expect("ensure NATS streams");

    let reader = NatsClient::connect(&nats_url(), false, None)
        .await
        .expect("connect reader NATS client");
    reader.ensure_streams().await.expect("ensure NATS streams");

    // Establish a fresh durable cursor at the current tail so historical
    // STATE_UPDATES traffic from the live environment does not contaminate
    // this test's reconnect assertion.
    let mut durable = reader
        .state_consumer(&durable_id)
        .await
        .expect("create durable state consumer");
    drain_until_idle(&mut durable, Duration::from_millis(250)).await;
    drop(durable);

    let first_event = sample_state_event(&publisher_id, &first_rel_path, &first_manifest, 1);
    publisher
        .publish_state_event(&first_event)
        .await
        .expect("publish first state event");

    let mut durable = reader
        .state_consumer(&durable_id)
        .await
        .expect("reopen durable state consumer");
    let replayed =
        recv_matching_event(&mut durable, &first_rel_path, Duration::from_secs(10)).await;
    assert!(
        matches_rel_path(&replayed, &first_rel_path),
        "durable consumer did not replay disconnected event"
    );
    drop(durable);

    let mut ephemeral = reader
        .state_consumer_ephemeral()
        .await
        .expect("create ephemeral state consumer");
    assert_no_matching_event(&mut ephemeral, &first_rel_path, Duration::from_secs(2)).await;

    let second_event = sample_state_event(&publisher_id, &second_rel_path, &second_manifest, 2);
    publisher
        .publish_state_event(&second_event)
        .await
        .expect("publish second state event");

    let seen = recv_matching_event(&mut ephemeral, &second_rel_path, Duration::from_secs(10)).await;
    assert!(
        matches_rel_path(&seen, &second_rel_path),
        "ephemeral consumer missed new event after attach"
    );
}

// ── Two-device sync simulation via NATS ──────────────────────────────────

#[tokio::test]
async fn neo_honey_two_device_sync_smoke() {
    if !live_enabled() {
        eprintln!("SKIP: TCFS_E2E_LIVE not set");
        return;
    }

    let op = live_operator().expect("S3 credentials required");
    let url = nats_url();
    let client = async_nats::connect(&url).await.expect("NATS connect");

    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    let test_id = uuid::Uuid::new_v4().to_string();
    let prefix = format!("{}/{}", NEO_HONEY_PREFIX, &test_id[..8]);

    // Subscribe to sync events before pushing
    let subject = format!("tcfs.sync.{}", prefix.replace('/', "."));
    let mut sub = client.subscribe(subject.clone()).await.expect("subscribe");

    // Device A: push file
    let content = b"synced from device A";
    let src_a = write_test_file(tmp_a.path(), "sync.txt", content);
    let mut state_a =
        tcfs_sync::state::StateCache::open(&tmp_a.path().join("state.db.json")).unwrap();

    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_a,
        &prefix,
        &mut state_a,
        None,
        NEO_DEVICE,
        Some("sync.txt"),
        None,
    )
    .await
    .expect("device A push");

    // Device A publishes sync event to NATS
    let event = serde_json::json!({
        "device": NEO_DEVICE,
        "action": "push",
        "path": "sync.txt",
        "manifest": upload.remote_path,
        "hash": upload.hash,
    });
    client
        .publish(subject.clone(), serde_json::to_vec(&event).unwrap().into())
        .await
        .expect("publish sync event");
    client.flush().await.expect("flush");

    // Device B: receive NATS event
    let msg = tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .expect("timeout waiting for sync event")
        .expect("no sync event");

    let received: serde_json::Value =
        serde_json::from_slice(&msg.payload).expect("parse sync event");
    assert_eq!(received["device"], NEO_DEVICE);
    assert_eq!(received["action"], "push");

    // Device B: pull the file using manifest from event
    let manifest_path = received["manifest"].as_str().expect("manifest path");
    let dst_b = tmp_b.path().join("sync.txt");

    let download = tcfs_sync::engine::download_file_with_device(
        &op,
        manifest_path,
        &dst_b,
        &prefix,
        None,
        HONEY_DEVICE,
        None,
        None,
    )
    .await
    .expect("device B pull");

    let pulled = std::fs::read(&dst_b).unwrap();
    assert_eq!(&pulled, content, "device B got different content");

    eprintln!(
        "neo-honey smoke verified: {neo} pushed {} bytes, {honey} pulled {} bytes via NATS",
        upload.bytes,
        download.bytes,
        neo = NEO_DEVICE,
        honey = HONEY_DEVICE,
    );

    // Cleanup
    let _ = op.delete(&upload.remote_path).await;
    let index_key = format!("{}/index/sync.txt", prefix);
    let _ = op.delete(&index_key).await;
}
