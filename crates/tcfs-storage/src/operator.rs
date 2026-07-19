//! OpenDAL Operator factory for tcfs storage backends

use anyhow::{Context, Result};
use opendal::raw::HttpClient;
use opendal::{ErrorKind, Operator};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tcfs_core::config::sanitize_http_endpoint_for_display;

/// Minimal config needed to build an operator
/// (full config lives in tcfs-core's StorageConfig)
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Permit credentials to be sent over plaintext HTTP.
    ///
    /// This is intentionally false by default. Callers may enable it only for
    /// isolated development or test endpoints.
    pub allow_insecure_http: bool,
    pub s3_connect_timeout_secs: u64,
    pub s3_pool_idle_timeout_secs: u64,
    pub s3_pool_max_idle_per_host: usize,
    pub s3_http1_only: bool,
    pub ca_cert_path: Option<PathBuf>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8333".to_string(),
            region: "us-east-1".to_string(),
            bucket: "tcfs".to_string(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            allow_insecure_http: false,
            s3_connect_timeout_secs: 0,
            s3_pool_idle_timeout_secs: 0,
            s3_pool_max_idle_per_host: 0,
            s3_http1_only: false,
            ca_cert_path: None,
        }
    }
}

/// Build an OpenDAL Operator for SeaweedFS S3 (or any S3-compatible endpoint)
///
/// Uses path-style addressing (default in opendal 0.55), which is required by
/// SeaweedFS and MinIO. Do NOT call enable_virtual_host_style() for these.
pub fn build_operator(cfg: &StorageConfig) -> Result<Operator> {
    build_operator_with_limits(cfg, 0)
}

/// Build an operator with optional concurrent-operation limiting.
///
/// If `max_concurrent > 0`, applies `ConcurrentLimitLayer` to cap inflight S3 ops.
pub fn build_operator_with_limits(cfg: &StorageConfig, max_concurrent: usize) -> Result<Operator> {
    validate_endpoint_transport(cfg)?;

    let http_client = build_s3_http_client(cfg)?;
    let operator = build_unlimited_s3_operator(cfg, http_client.clone())?;

    if max_concurrent == 0 {
        return Ok(operator);
    }

    // Build a separate accessor for the conformance race before applying the
    // application's operation and HTTP semaphores. Sharing the pre-layer
    // accessor is insufficient because ConcurrentLimitLayer mutates its
    // AccessorInfo HTTP client in place; max_concurrent=1 would then serialize
    // the two contenders and let a server-side check-then-write bug pass.
    let probe_operator = build_unlimited_s3_operator(cfg, http_client)?;
    tracing::info!(
        max_concurrent,
        "S3 concurrent operation and HTTP request limits enabled"
    );
    let operator = operator.layer(
        opendal::layers::ConcurrentLimitLayer::new(max_concurrent)
            .with_http_concurrent_limit(max_concurrent),
    );
    register_conditional_write_probe_route(&operator, probe_operator)?;
    Ok(operator)
}

fn build_unlimited_s3_operator(
    cfg: &StorageConfig,
    http_client: Option<HttpClient>,
) -> Result<Operator> {
    // opendal 0.55: S3 builder uses consuming pattern (methods take `self`, return `Self`).
    let mut builder = opendal::services::S3::default()
        .endpoint(&cfg.endpoint)
        .region(&cfg.region)
        .bucket(&cfg.bucket)
        .access_key_id(&cfg.access_key_id)
        .secret_access_key(&cfg.secret_access_key)
        // Tell OpenDAL to expose S3 object-version operations. This does not
        // enable bucket-side versioning: unversioned/suspended buckets return
        // no usable generation and orphan GC remains fail-closed.
        .enable_versioning(true);
    // Note: path-style addressing is the default — no enable_virtual_host_style() needed.
    if let Some(http_client) = http_client {
        #[allow(deprecated)]
        {
            builder = builder.http_client(http_client);
        }
    }

    Ok(Operator::new(builder)
        .context("creating OpenDAL S3 operator")?
        .layer(opendal::layers::LoggingLayer::default())
        .layer(
            opendal::layers::RetryLayer::new()
                .with_max_times(5)
                .with_factor(2.0)
                .with_jitter(),
        )
        .finish())
}

const CONDITIONAL_WRITE_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const CONDITIONAL_WRITE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

fn normalize_probe_prefix(prefix: &str) -> Result<String> {
    let prefix = prefix.trim_matches('/');
    anyhow::ensure!(
        prefix.is_empty()
            || (!prefix.contains('\\')
                && !prefix.chars().any(char::is_control)
                && !prefix.split('/').any(|component| component.is_empty()
                    || component == "."
                    || component == "..")),
        "conditional-write probe prefix is not a safe relative object-key prefix: {prefix:?}"
    );
    Ok(prefix.to_string())
}

enum ConditionalRaceWinner<T> {
    Left(T),
    Right(T),
}

#[derive(Clone, Debug, Default)]
struct ProbeOwnership {
    created: bool,
    versions: Vec<String>,
}

fn record_probe_write(ownership: &Arc<Mutex<ProbeOwnership>>, metadata: &opendal::Metadata) {
    let mut ownership = ownership
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ownership.created = true;
    if let Some(version) = metadata
        .version()
        .filter(|version| !version.is_empty())
        .filter(|version| !ownership.versions.iter().any(|known| known == version))
    {
        ownership.versions.push(version.to_owned());
    }
}

fn exactly_one_conditional_winner<T>(
    left: opendal::Result<T>,
    right: opendal::Result<T>,
    expected_rejection: impl Fn(ErrorKind) -> bool,
    operation: &str,
    probe_key: &str,
) -> Result<ConditionalRaceWinner<T>> {
    match (left, right) {
        (Ok(left), Err(right)) if expected_rejection(right.kind()) => {
            Ok(ConditionalRaceWinner::Left(left))
        }
        (Err(left), Ok(right)) if expected_rejection(left.kind()) => {
            Ok(ConditionalRaceWinner::Right(right))
        }
        (Ok(_), Ok(_)) => anyhow::bail!(
            "storage endpoint accepted more than one winner during {operation}; conditional writes are not atomic: {probe_key}"
        ),
        (Err(left), Err(right))
            if expected_rejection(left.kind()) && expected_rejection(right.kind()) =>
        {
            anyhow::bail!(
                "storage endpoint rejected every contender during {operation}; expected exactly one winner: {probe_key}"
            )
        }
        (Ok(_), Err(error)) | (Err(error), Ok(_)) => Err(anyhow::Error::new(error))
            .with_context(|| format!("unexpected conditional-write failure during {operation}: {probe_key}")),
        (Err(left), Err(right)) => anyhow::bail!(
            "storage endpoint failed both contenders during {operation}: {probe_key}; left={left}; right={right}"
        ),
    }
}

async fn run_conditional_write_probe(
    op: &Operator,
    probe_key: &str,
    probe_timeout: Duration,
    cleanup_timeout: Duration,
) -> Result<()> {
    let ownership = Arc::new(Mutex::new(ProbeOwnership::default()));
    let probe_result = match tokio::time::timeout(probe_timeout, async {
        let create_left = b"tcfs-conditional-create-left".to_vec();
        let create_right = b"tcfs-conditional-create-right".to_vec();

        // An advertised If-None-Match implementation is insufficient: the
        // no-loss protocol requires the absence check and write to be one
        // atomic operation. Distinct contenders make a double winner visible.
        let left_ownership = ownership.clone();
        let right_ownership = ownership.clone();
        let (left_create, right_create) = tokio::join!(
            async {
                let result = op
                    .write_with(probe_key, create_left.clone())
                    .if_not_exists(true)
                    .await;
                if let Ok(metadata) = &result {
                    record_probe_write(&left_ownership, metadata);
                }
                result
            },
            async {
                let result = op
                    .write_with(probe_key, create_right.clone())
                    .if_not_exists(true)
                    .await;
                if let Ok(metadata) = &result {
                    record_probe_write(&right_ownership, metadata);
                }
                result
            },
        );
        let create_winner = exactly_one_conditional_winner(
            left_create,
            right_create,
            |kind| {
                matches!(
                    kind,
                    ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists
                )
            },
            "concurrent create-if-absent race",
            probe_key,
        )?;
        let (initial, first_write) = match create_winner {
            ConditionalRaceWinner::Left(metadata) => (create_left, metadata),
            ConditionalRaceWinner::Right(metadata) => (create_right, metadata),
        };

        let initial_etag = match first_write.etag().filter(|etag| !etag.is_empty()) {
            Some(etag) => etag.to_owned(),
            None => op
                .stat(probe_key)
                .await
                .with_context(|| format!("statting conditional-write probe: {probe_key}"))?
                .etag()
                .filter(|etag| !etag.is_empty())
                .map(str::to_owned)
                .context("conditional-write probe object has no usable ETag")?,
        };

        let bound = op
            .read_with(probe_key)
            .if_match(&initial_etag)
            .await
            .with_context(|| format!("testing ETag-bound probe read: {probe_key}"))?;
        anyhow::ensure!(
            bound.to_vec() == initial,
            "conditional-write probe changed before its ETag-bound read: {probe_key}"
        );

        let update_left = b"tcfs-conditional-update-left".to_vec();
        let update_right = b"tcfs-conditional-update-right".to_vec();
        let left_ownership = ownership.clone();
        let right_ownership = ownership.clone();
        let (left_update, right_update) = tokio::join!(
            async {
                let result = op
                    .write_with(probe_key, update_left.clone())
                    .if_match(&initial_etag)
                    .await;
                if let Ok(metadata) = &result {
                    record_probe_write(&left_ownership, metadata);
                }
                result
            },
            async {
                let result = op
                    .write_with(probe_key, update_right.clone())
                    .if_match(&initial_etag)
                    .await;
                if let Ok(metadata) = &result {
                    record_probe_write(&right_ownership, metadata);
                }
                result
            },
        );
        let update_winner = exactly_one_conditional_winner(
            left_update,
            right_update,
            |kind| kind == ErrorKind::ConditionNotMatch,
            "concurrent same-ETag update race",
            probe_key,
        )?;
        let expected_final = match update_winner {
            ConditionalRaceWinner::Left(_) => update_left,
            ConditionalRaceWinner::Right(_) => update_right,
        };

        // A valid conditional read alone does not prove the server honors
        // If-Match. The now-stale ETag must be actively rejected.
        match op.read_with(probe_key).if_match(&initial_etag).await {
            Err(error) if error.kind() == ErrorKind::ConditionNotMatch => {}
            Err(error) => {
                return Err(anyhow::Error::new(error))
                    .with_context(|| format!("testing stale ETag read rejection: {probe_key}"))
            }
            Ok(_) => anyhow::bail!(
                "storage endpoint ignored stale If-Match on conditional read: {probe_key}"
            ),
        }

        let final_bytes = op
            .read(probe_key)
            .await
            .with_context(|| format!("reading final conditional-write probe: {probe_key}"))?
            .to_vec();
        anyhow::ensure!(
            final_bytes == expected_final,
            "storage endpoint did not preserve the sole conditional-update winner: {probe_key}"
        );
        Ok(())
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "conditional-write semantics probe timed out after {probe_timeout:?}: {probe_key}"
        )),
    };

    // Never delete an object when both create contenders failed: an
    // astronomically unlikely key collision, or an ambiguous failed request,
    // may mean that object belongs to somebody else. A known winner is the
    // minimum evidence that this invocation owns cleanup.
    let ownership = ownership
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if ownership.created {
        let cleanup = async {
            if ownership.versions.is_empty() {
                op.delete(probe_key).await
            } else {
                for version in &ownership.versions {
                    op.delete_with(probe_key).version(version).await?;
                }
                Ok(())
            }
        };
        match tokio::time::timeout(cleanup_timeout, cleanup).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                path = %probe_key,
                error = %error,
                "failed to remove conditional-write capability probe"
            ),
            Err(_) => tracing::warn!(
                path = %probe_key,
                timeout_ms = cleanup_timeout.as_millis() as u64,
                "timed out removing conditional-write capability probe"
            ),
        }
    }

    probe_result
}

/// Verify that the live object-store endpoint actually enforces the conditional
/// operations used by the path-index publication protocol.
///
/// OpenDAL capabilities describe requests the S3 adapter can emit; they cannot
/// prove that an S3-compatible server honors those headers. This bounded probe
/// requires exactly one winner in concurrent create-if-absent and same-ETag
/// update races, and verifies that a stale ETag is rejected on reads.
pub async fn verify_conditional_write_semantics(op: &Operator, prefix: &str) -> Result<()> {
    let prefix = normalize_probe_prefix(prefix)?;
    let probe_op = conditional_write_probe_operator(op)?;
    let op = &probe_op;
    let capability = op.info().full_capability();
    anyhow::ensure!(
        capability.write_with_if_not_exists
            && capability.read_with_if_match
            && capability.write_with_if_match,
        "storage backend does not advertise the conditional read/write operations required for atomic index publication"
    );

    let probe_name = format!(
        ".tcfs-capability-probes/conditional-write-{}",
        uuid::Uuid::new_v4().hyphenated()
    );
    let probe_key = if prefix.is_empty() {
        probe_name
    } else {
        format!("{prefix}/{probe_name}")
    };

    run_conditional_write_probe(
        op,
        &probe_key,
        CONDITIONAL_WRITE_PROBE_TIMEOUT,
        CONDITIONAL_WRITE_CLEANUP_TIMEOUT,
    )
    .await
}

type WeakAccessor = Weak<dyn opendal::raw::AccessDyn>;

struct ConditionalWriteProbeRoute {
    application: WeakAccessor,
    /// Separate unthrottled accessor for the live concurrent conformance race.
    /// Entries are removed lazily once the application accessor is gone.
    probe: Operator,
}

fn conditional_write_probe_routes() -> &'static Mutex<Vec<ConditionalWriteProbeRoute>> {
    static ROUTES: OnceLock<Mutex<Vec<ConditionalWriteProbeRoute>>> = OnceLock::new();
    ROUTES.get_or_init(|| Mutex::new(Vec::new()))
}

fn register_conditional_write_probe_route(application: &Operator, probe: Operator) -> Result<()> {
    let application = Arc::downgrade(application.inner());
    let mut routes = conditional_write_probe_routes()
        .lock()
        .map_err(|_| anyhow::anyhow!("conditional-write probe route registry is poisoned"))?;
    routes.retain(|route| route.application.upgrade().is_some());
    if let Some(route) = routes
        .iter_mut()
        .find(|route| Weak::ptr_eq(&route.application, &application))
    {
        route.probe = probe;
    } else {
        routes.push(ConditionalWriteProbeRoute { application, probe });
    }
    Ok(())
}

fn conditional_write_probe_operator(application: &Operator) -> Result<Operator> {
    let application_accessor = Arc::downgrade(application.inner());
    let mut routes = conditional_write_probe_routes()
        .lock()
        .map_err(|_| anyhow::anyhow!("conditional-write probe route registry is poisoned"))?;
    routes.retain(|route| route.application.upgrade().is_some());
    Ok(routes
        .iter()
        .find(|route| Weak::ptr_eq(&route.application, &application_accessor))
        .map(|route| route.probe.clone())
        .unwrap_or_else(|| application.clone()))
}

fn memory_conditional_write_emulations() -> &'static Mutex<Vec<WeakAccessor>> {
    static EMULATIONS: OnceLock<Mutex<Vec<WeakAccessor>>> = OnceLock::new();
    EMULATIONS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register one exact OpenDAL Memory accessor whose caller provides equivalent
/// process-local conditional-write emulation for tests.
///
/// This exemption is deliberately accessor-scoped. It must never be inferred
/// from the Memory scheme because an unrelated Memory operator would then skip
/// the live publication-safety gate without providing the matching emulation.
#[doc(hidden)]
pub fn register_memory_conditional_write_emulation_for_tests(op: &Operator) -> Result<()> {
    anyhow::ensure!(
        op.info().scheme() == "memory",
        "conditional-write test emulation is restricted to OpenDAL Memory accessors"
    );

    let accessor = Arc::downgrade(op.inner());
    let mut registered = memory_conditional_write_emulations()
        .lock()
        .map_err(|_| anyhow::anyhow!("conditional-write emulation registry is poisoned"))?;
    registered.retain(|candidate| candidate.upgrade().is_some());
    if !registered
        .iter()
        .any(|candidate| Weak::ptr_eq(candidate, &accessor))
    {
        registered.push(accessor);
    }
    Ok(())
}

fn memory_conditional_write_emulation_is_registered(op: &Operator) -> Result<bool> {
    if op.info().scheme() != "memory" {
        return Ok(false);
    }

    let accessor = Arc::downgrade(op.inner());
    let mut registered = memory_conditional_write_emulations()
        .lock()
        .map_err(|_| anyhow::anyhow!("conditional-write emulation registry is poisoned"))?;
    registered.retain(|candidate| candidate.upgrade().is_some());
    Ok(registered
        .iter()
        .any(|candidate| Weak::ptr_eq(candidate, &accessor)))
}

/// Report whether this exact Memory accessor was explicitly registered for
/// process-local conditional-write emulation by a test harness.
#[doc(hidden)]
pub fn memory_conditional_write_emulation_is_registered_for_tests(op: &Operator) -> Result<bool> {
    memory_conditional_write_emulation_is_registered(op)
}

struct ConditionalWriteVerification {
    accessor: WeakAccessor,
    prefix: String,
    verified: Arc<tokio::sync::OnceCell<()>>,
}

fn conditional_write_verifications() -> &'static Mutex<Vec<ConditionalWriteVerification>> {
    static VERIFICATIONS: OnceLock<Mutex<Vec<ConditionalWriteVerification>>> = OnceLock::new();
    VERIFICATIONS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Verify the live conditional-write contract once for this exact operator and
/// prefix, retrying on a later call when a transient probe attempt fails.
///
/// The cache is bound to the underlying OpenDAL accessor identity rather than
/// scheme/bucket metadata because S3 operator metadata does not include the
/// endpoint. Dead accessors are discarded, so allocator address reuse cannot
/// inherit another operator's verification.
///
/// Tests that provide companion process-local conditional-write emulation may
/// explicitly register one exact Memory accessor. No other Memory accessor is
/// exempt from the live contract check.
pub async fn ensure_conditional_write_semantics(op: &Operator, prefix: &str) -> Result<()> {
    let prefix = normalize_probe_prefix(prefix)?;
    if memory_conditional_write_emulation_is_registered(op)? {
        return Ok(());
    }

    let accessor = Arc::downgrade(op.inner());
    let verified = {
        let mut verifications = conditional_write_verifications()
            .lock()
            .map_err(|_| anyhow::anyhow!("conditional-write verification cache is poisoned"))?;
        verifications.retain(|entry| entry.accessor.upgrade().is_some());
        if let Some(entry) = verifications
            .iter()
            .find(|entry| entry.prefix == prefix && Weak::ptr_eq(&entry.accessor, &accessor))
        {
            entry.verified.clone()
        } else {
            let verified = Arc::new(tokio::sync::OnceCell::new());
            verifications.push(ConditionalWriteVerification {
                accessor,
                prefix: prefix.clone(),
                verified: verified.clone(),
            });
            verified
        }
    };

    verified
        .get_or_try_init(|| verify_conditional_write_semantics(op, &prefix))
        .await?;
    Ok(())
}

fn validate_endpoint_transport(cfg: &StorageConfig) -> Result<()> {
    let endpoint_display = sanitize_http_endpoint_for_display(&cfg.endpoint);
    let endpoint = reqwest::Url::parse(&cfg.endpoint)
        .with_context(|| format!("parsing S3 endpoint URL {endpoint_display}"))?;

    if let Some(warning) = insecure_transport_warning(endpoint.scheme(), cfg.allow_insecure_http) {
        tracing::warn!(endpoint = %endpoint_display, "{warning}");
    }

    match endpoint.scheme() {
        "https" => Ok(()),
        "http" if cfg.allow_insecure_http => Ok(()),
        "http" => anyhow::bail!(
            "S3 endpoint uses plaintext HTTP ({}). Use an HTTPS endpoint. For isolated development or tests only, explicitly set storage.enforce_tls = false (tcfs config) or allow_insecure_http = true (low-level client).",
            endpoint_display
        ),
        _ => anyhow::bail!(
            "unsupported S3 endpoint scheme in {}; HTTPS is required",
            endpoint_display
        ),
    }
}

fn insecure_transport_warning(scheme: &str, allow_insecure_http: bool) -> Option<&'static str> {
    if !allow_insecure_http {
        return None;
    }
    match scheme {
        "http" => Some(
            "S3 endpoint uses explicitly allowed plaintext HTTP; credentials are transmitted \
             unencrypted. This mode is for isolated development and tests only.",
        ),
        "https" => Some(
            "S3 insecure-HTTP compatibility is enabled for an HTTPS endpoint; the first hop uses \
             TLS, but redirects may downgrade to plaintext HTTP. Disable this development/test \
             opt-in to enforce HTTPS for the complete request chain.",
        ),
        _ => None,
    }
}

fn build_s3_http_client(cfg: &StorageConfig) -> Result<Option<HttpClient>> {
    // OpenDAL requires redirect support, while reqwest's default policy also
    // permits an HTTPS endpoint to redirect to plaintext HTTP. Install a
    // bounded policy for every operator so `allow_insecure_http = false`
    // covers the complete request chain, not only the configured first hop.
    let allow_insecure_http = cfg.allow_insecure_http;
    let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many S3 redirects");
        }
        if redirect_scheme_allowed(attempt.url().scheme(), allow_insecure_http) {
            attempt.follow()
        } else {
            attempt.error("S3 redirect to insecure or unsupported transport rejected")
        }
    });
    let mut builder = reqwest::Client::builder().redirect(redirect_policy);
    if let Some(path) = &cfg.ca_cert_path {
        let pem = std::fs::read(path)
            .with_context(|| format!("reading S3 CA certificate {}", path.display()))?;
        let cert = reqwest::Certificate::from_pem(&pem)
            .with_context(|| format!("parsing S3 CA certificate {}", path.display()))?;
        builder = builder.add_root_certificate(cert);
    }
    if cfg.s3_connect_timeout_secs > 0 {
        builder = builder.connect_timeout(Duration::from_secs(cfg.s3_connect_timeout_secs));
    }
    if cfg.s3_pool_idle_timeout_secs > 0 {
        builder = builder.pool_idle_timeout(Duration::from_secs(cfg.s3_pool_idle_timeout_secs));
    }
    if cfg.s3_pool_max_idle_per_host > 0 {
        builder = builder.pool_max_idle_per_host(cfg.s3_pool_max_idle_per_host);
    }
    if cfg.s3_http1_only {
        builder = builder.http1_only();
    }

    let client = builder.build().context("building bounded S3 HTTP client")?;
    tracing::info!(
        s3_connect_timeout_secs = cfg.s3_connect_timeout_secs,
        s3_pool_idle_timeout_secs = cfg.s3_pool_idle_timeout_secs,
        s3_pool_max_idle_per_host = cfg.s3_pool_max_idle_per_host,
        s3_http1_only = cfg.s3_http1_only,
        ca_cert_path = cfg
            .ca_cert_path
            .as_ref()
            .map(|path| path.display().to_string()),
        allow_insecure_http = cfg.allow_insecure_http,
        "S3 HTTP client security and transport controls enabled"
    );
    Ok(Some(HttpClient::with(client)))
}

fn redirect_scheme_allowed(scheme: &str, allow_insecure_http: bool) -> bool {
    scheme == "https" || (allow_insecure_http && scheme == "http")
}

/// Build an operator from tcfs-core config + loaded credentials.
///
/// HTTPS is required by default. A plaintext HTTP endpoint is accepted only
/// when the core config explicitly sets `enforce_tls = false` for isolated
/// development or tests.
pub fn build_from_core_config(
    storage: &tcfs_core::config::StorageConfig,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<Operator> {
    build_operator_with_limits(
        &StorageConfig {
            endpoint: storage.endpoint.clone(),
            region: storage.region.clone(),
            bucket: storage.bucket.clone(),
            access_key_id: access_key_id.to_string(),
            secret_access_key: secret_access_key.to_string(),
            allow_insecure_http: !storage.enforce_tls,
            s3_connect_timeout_secs: storage.s3_connect_timeout_secs,
            s3_pool_idle_timeout_secs: storage.s3_pool_idle_timeout_secs,
            s3_pool_max_idle_per_host: storage.s3_pool_max_idle_per_host,
            s3_http1_only: storage.s3_http1_only,
            ca_cert_path: storage.ca_cert_path.clone(),
        },
        storage.max_concurrent_ops,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::raw::{oio, Access, AccessorInfo, OpDelete, OpRead, OpStat, OpWrite};
    use opendal::raw::{RpDelete, RpRead, RpStat, RpWrite};
    use opendal::{Buffer, Capability, EntryMode, Metadata, OperatorBuilder};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Clone, Debug)]
    struct TestObject {
        bytes: Vec<u8>,
        etag: String,
    }

    #[derive(Clone, Copy, Debug)]
    struct TestConditionalBehavior {
        honor_create_conditions: bool,
        honor_update_conditions: bool,
        honor_read_conditions: bool,
        return_versions: bool,
        hang_writes: bool,
        hang_write_attempt: Option<u64>,
        hang_deletes: bool,
    }

    impl TestConditionalBehavior {
        const CORRECT: Self = Self {
            honor_create_conditions: true,
            honor_update_conditions: true,
            honor_read_conditions: true,
            return_versions: false,
            hang_writes: false,
            hang_write_attempt: None,
            hang_deletes: false,
        };

        const IGNORE_CREATES: Self = Self {
            honor_create_conditions: false,
            honor_update_conditions: true,
            honor_read_conditions: true,
            return_versions: false,
            hang_writes: false,
            hang_write_attempt: None,
            hang_deletes: false,
        };

        const IGNORE_UPDATES: Self = Self {
            honor_create_conditions: true,
            honor_update_conditions: false,
            honor_read_conditions: true,
            return_versions: false,
            hang_writes: false,
            hang_write_attempt: None,
            hang_deletes: false,
        };

        const IGNORE_READS: Self = Self {
            honor_create_conditions: true,
            honor_update_conditions: true,
            honor_read_conditions: false,
            return_versions: false,
            hang_writes: false,
            hang_write_attempt: None,
            hang_deletes: false,
        };

        const HANG_WRITES: Self = Self {
            honor_create_conditions: true,
            honor_update_conditions: true,
            honor_read_conditions: true,
            return_versions: false,
            hang_writes: true,
            hang_write_attempt: None,
            hang_deletes: false,
        };

        const HANG_SECOND_WRITE: Self = Self {
            honor_create_conditions: true,
            honor_update_conditions: true,
            honor_read_conditions: true,
            return_versions: false,
            hang_writes: false,
            hang_write_attempt: Some(1),
            hang_deletes: false,
        };

        const VERSIONED_CORRECT: Self = Self {
            honor_create_conditions: true,
            honor_update_conditions: true,
            honor_read_conditions: true,
            return_versions: true,
            hang_writes: false,
            hang_write_attempt: None,
            hang_deletes: false,
        };

        const HANG_DELETES: Self = Self {
            honor_create_conditions: true,
            honor_update_conditions: true,
            honor_read_conditions: true,
            return_versions: false,
            hang_writes: false,
            hang_write_attempt: None,
            hang_deletes: true,
        };
    }

    #[derive(Clone, Debug)]
    struct TestConditionalBackend {
        objects: Arc<Mutex<HashMap<String, TestObject>>>,
        next_etag: Arc<AtomicU64>,
        write_attempts: Arc<AtomicU64>,
        deleted_versions: Arc<Mutex<Vec<Option<String>>>>,
        behavior: TestConditionalBehavior,
        info: Arc<AccessorInfo>,
    }

    impl TestConditionalBackend {
        fn object(&self, path: &str) -> Option<TestObject> {
            self.objects.lock().unwrap().get(path).cloned()
        }

        fn object_count(&self) -> usize {
            self.objects.lock().unwrap().len()
        }

        fn write_attempts(&self) -> u64 {
            self.write_attempts.load(Ordering::SeqCst)
        }

        fn deleted_versions(&self) -> Vec<Option<String>> {
            self.deleted_versions.lock().unwrap().clone()
        }
    }

    impl Access for TestConditionalBackend {
        type Reader = Buffer;
        type Writer = oio::OneShotWriter<TestConditionalWriter>;
        type Lister = ();
        type Deleter = oio::OneShotDeleter<TestConditionalDeleter>;

        fn info(&self) -> Arc<AccessorInfo> {
            self.info.clone()
        }

        async fn stat(&self, path: &str, _: OpStat) -> opendal::Result<RpStat> {
            let object = self.object(path).ok_or_else(|| {
                opendal::Error::new(ErrorKind::NotFound, "test object does not exist")
            })?;
            let metadata = Metadata::new(EntryMode::FILE)
                .with_content_length(object.bytes.len() as u64)
                .with_etag(object.etag);
            Ok(RpStat::new(metadata))
        }

        async fn read(&self, path: &str, args: OpRead) -> opendal::Result<(RpRead, Self::Reader)> {
            let object = self.object(path).ok_or_else(|| {
                opendal::Error::new(ErrorKind::NotFound, "test object does not exist")
            })?;
            if self.behavior.honor_read_conditions
                && args
                    .if_match()
                    .is_some_and(|expected| expected != object.etag.as_str())
            {
                return Err(opendal::Error::new(
                    ErrorKind::ConditionNotMatch,
                    "test conditional read rejected stale ETag",
                ));
            }
            Ok((RpRead::new(), Buffer::from(object.bytes)))
        }

        async fn write(
            &self,
            path: &str,
            args: OpWrite,
        ) -> opendal::Result<(RpWrite, Self::Writer)> {
            Ok((
                RpWrite::new(),
                oio::OneShotWriter::new(TestConditionalWriter {
                    path: path.to_string(),
                    args,
                    objects: self.objects.clone(),
                    next_etag: self.next_etag.clone(),
                    write_attempts: self.write_attempts.clone(),
                    behavior: self.behavior,
                }),
            ))
        }

        async fn delete(&self) -> opendal::Result<(RpDelete, Self::Deleter)> {
            Ok((
                RpDelete::default(),
                oio::OneShotDeleter::new(TestConditionalDeleter {
                    objects: self.objects.clone(),
                    deleted_versions: self.deleted_versions.clone(),
                    hang_deletes: self.behavior.hang_deletes,
                }),
            ))
        }
    }

    struct TestConditionalWriter {
        path: String,
        args: OpWrite,
        objects: Arc<Mutex<HashMap<String, TestObject>>>,
        next_etag: Arc<AtomicU64>,
        write_attempts: Arc<AtomicU64>,
        behavior: TestConditionalBehavior,
    }

    impl oio::OneShotWrite for TestConditionalWriter {
        async fn write_once(&self, bytes: Buffer) -> opendal::Result<Metadata> {
            let attempt = self.write_attempts.fetch_add(1, Ordering::SeqCst);
            if self.behavior.hang_writes || self.behavior.hang_write_attempt == Some(attempt) {
                return std::future::pending::<opendal::Result<Metadata>>().await;
            }

            let mut objects = self.objects.lock().unwrap();
            let current_etag = objects.get(&self.path).map(|object| object.etag.as_str());
            if self.behavior.honor_create_conditions
                && self.args.if_not_exists()
                && current_etag.is_some()
            {
                return Err(opendal::Error::new(
                    ErrorKind::ConditionNotMatch,
                    "test create-if-absent rejected existing object",
                ));
            }
            if self.behavior.honor_update_conditions
                && self
                    .args
                    .if_match()
                    .is_some_and(|expected| current_etag != Some(expected))
            {
                return Err(opendal::Error::new(
                    ErrorKind::ConditionNotMatch,
                    "test conditional write rejected stale ETag",
                ));
            }

            let generation = self.next_etag.fetch_add(1, Ordering::SeqCst);
            let etag = format!("\"test-etag-{generation}\"");
            let bytes = bytes.to_vec();
            objects.insert(
                self.path.clone(),
                TestObject {
                    bytes: bytes.clone(),
                    etag: etag.clone(),
                },
            );
            let mut metadata = Metadata::new(EntryMode::FILE)
                .with_content_length(bytes.len() as u64)
                .with_etag(etag);
            if self.behavior.return_versions {
                metadata = metadata.with_version(format!("test-version-{generation}"));
            }
            Ok(metadata)
        }
    }

    struct TestConditionalDeleter {
        objects: Arc<Mutex<HashMap<String, TestObject>>>,
        deleted_versions: Arc<Mutex<Vec<Option<String>>>>,
        hang_deletes: bool,
    }

    impl oio::OneShotDelete for TestConditionalDeleter {
        async fn delete_once(&self, path: String, args: OpDelete) -> opendal::Result<()> {
            if self.hang_deletes {
                return std::future::pending::<opendal::Result<()>>().await;
            }
            self.deleted_versions
                .lock()
                .unwrap()
                .push(args.version().map(str::to_owned));
            self.objects.lock().unwrap().remove(&path);
            Ok(())
        }
    }

    fn conditional_test_operator(
        behavior: TestConditionalBehavior,
    ) -> (Operator, TestConditionalBackend) {
        let info = AccessorInfo::default();
        info.set_scheme("conditional-test")
            .set_root("/")
            .set_name("conditional-test")
            .set_native_capability(Capability {
                stat: true,
                read: true,
                read_with_if_match: true,
                write: true,
                write_can_empty: true,
                write_with_if_match: true,
                write_with_if_not_exists: true,
                delete: true,
                delete_with_version: behavior.return_versions,
                ..Default::default()
            });
        let backend = TestConditionalBackend {
            objects: Arc::new(Mutex::new(HashMap::new())),
            next_etag: Arc::new(AtomicU64::new(1)),
            write_attempts: Arc::new(AtomicU64::new(0)),
            deleted_versions: Arc::new(Mutex::new(Vec::new())),
            behavior,
            info: Arc::new(info),
        };
        (OperatorBuilder::new(backend.clone()).finish(), backend)
    }

    #[tokio::test]
    async fn conditional_write_probe_rejects_backend_without_required_capabilities() {
        let op = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();

        let error = verify_conditional_write_semantics(&op, "tenant-a")
            .await
            .expect_err("memory backend does not advertise the S3 publication contract");
        assert!(
            error.to_string().contains("does not advertise"),
            "{error:#}"
        );
        assert!(
            op.list("tenant-a/.tcfs-capability-probes/")
                .await
                .unwrap()
                .is_empty(),
            "capability rejection must not leave a probe object"
        );
    }

    #[tokio::test]
    async fn cached_verifier_does_not_trust_memory_as_a_live_endpoint() {
        let op = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();

        let error = ensure_conditional_write_semantics(&op, "/tenant-a/")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("does not advertise"),
            "{error:#}"
        );
        let error = ensure_conditional_write_semantics(&op, "tenant-a/../other")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("safe relative"), "{error:#}");
    }

    #[tokio::test]
    async fn memory_emulation_registration_is_exact_accessor_scoped() {
        let registered = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        let unregistered = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();

        register_memory_conditional_write_emulation_for_tests(&registered).unwrap();
        ensure_conditional_write_semantics(&registered, "/tenant/")
            .await
            .unwrap();

        let error = ensure_conditional_write_semantics(&unregistered, "tenant")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("does not advertise"),
            "a distinct Memory accessor inherited another accessor's exemption: {error:#}"
        );

        let error = ensure_conditional_write_semantics(&registered, "tenant/../other")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("safe relative"),
            "registration must not bypass prefix validation: {error:#}"
        );
    }

    #[test]
    fn memory_emulation_registration_rejects_non_memory_accessors() {
        let (op, _) = conditional_test_operator(TestConditionalBehavior::CORRECT);
        let error = register_memory_conditional_write_emulation_for_tests(&op).unwrap_err();
        assert!(
            error.to_string().contains("restricted to OpenDAL Memory"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn conditional_probe_route_is_exact_application_accessor_scoped() {
        let application = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        let unrelated = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        let (probe, _) = conditional_test_operator(TestConditionalBehavior::CORRECT);

        register_conditional_write_probe_route(&application, probe.clone()).unwrap();

        let selected = conditional_write_probe_operator(&application).unwrap();
        assert!(Arc::ptr_eq(selected.inner(), probe.inner()));
        let selected_for_clone = conditional_write_probe_operator(&application.clone()).unwrap();
        assert!(Arc::ptr_eq(selected_for_clone.inner(), probe.inner()));

        let unrelated_selected = conditional_write_probe_operator(&unrelated).unwrap();
        assert!(Arc::ptr_eq(unrelated_selected.inner(), unrelated.inner()));

        verify_conditional_write_semantics(&application, "tenant")
            .await
            .expect("public verification must bypass the application's local limiter route");
    }

    #[test]
    fn conditional_probe_prefix_is_canonical_and_traversal_free() {
        assert_eq!(
            normalize_probe_prefix("/tenant/nested/").unwrap(),
            "tenant/nested"
        );
        assert_eq!(normalize_probe_prefix("/").unwrap(), "");
        for invalid in [
            "tenant//nested",
            "tenant/../other",
            "tenant\\other",
            "tenant\nother",
        ] {
            assert!(
                normalize_probe_prefix(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[tokio::test]
    async fn conditional_probe_accepts_atomic_backend_and_cleans_up() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::CORRECT);

        run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap();

        assert_eq!(backend.object_count(), 0);
    }

    #[tokio::test]
    async fn conditional_probe_cleans_every_successful_version_without_a_delete_marker() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::VERSIONED_CORRECT);

        run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap();

        let deleted = backend.deleted_versions();
        assert_eq!(
            deleted.len(),
            2,
            "create and update versions must be removed"
        );
        assert!(
            deleted.iter().all(Option::is_some),
            "versioned cleanup must never issue an unversioned delete marker: {deleted:?}"
        );
        assert_eq!(backend.object_count(), 0);
    }

    #[tokio::test]
    async fn conditional_probe_rejects_backend_that_ignores_atomic_creates() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::IGNORE_CREATES);

        let error = run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(
            error.to_string().contains("more than one winner"),
            "{error:#}"
        );
        assert_eq!(
            backend.object_count(),
            0,
            "owned failed probe must be cleaned up"
        );
    }

    #[tokio::test]
    async fn conditional_probe_rejects_backend_that_ignores_atomic_updates() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::IGNORE_UPDATES);

        let error = run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(
            error.to_string().contains("more than one winner"),
            "{error:#}"
        );
        assert_eq!(
            backend.object_count(),
            0,
            "owned failed probe must be cleaned up"
        );
    }

    #[tokio::test]
    async fn conditional_probe_rejects_backend_that_ignores_stale_reads() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::IGNORE_READS);

        let error = run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(
            error.to_string().contains("ignored stale If-Match"),
            "{error:#}"
        );
        assert_eq!(
            backend.object_count(),
            0,
            "owned failed probe must be cleaned up"
        );
    }

    #[tokio::test]
    async fn conditional_probe_does_not_delete_a_preexisting_collision() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::CORRECT);
        op.write("tenant/probe", b"preexisting".to_vec())
            .await
            .unwrap();

        let error = run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(
            error.to_string().contains("rejected every contender"),
            "{error:#}"
        );
        assert_eq!(
            backend.object("tenant/probe").unwrap().bytes,
            b"preexisting".to_vec(),
            "a probe that observed no create winner must not clean up another object"
        );
    }

    #[tokio::test]
    async fn conditional_probe_timeout_is_bounded() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::HANG_WRITES);

        let error = run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_millis(10),
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert_eq!(backend.object_count(), 0);
    }

    #[tokio::test]
    async fn conditional_probe_cleans_up_a_success_observed_before_peer_timeout() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::HANG_SECOND_WRITE);

        let error = run_conditional_write_probe(
            &op,
            "tenant/probe",
            Duration::from_millis(10),
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert_eq!(
            backend.object_count(),
            0,
            "a completed contender must record cleanup ownership before join returns"
        );
    }

    #[tokio::test]
    async fn conditional_probe_cleanup_timeout_is_bounded() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::HANG_DELETES);

        tokio::time::timeout(
            Duration::from_secs(1),
            run_conditional_write_probe(
                &op,
                "tenant/probe",
                Duration::from_secs(1),
                Duration::from_millis(10),
            ),
        )
        .await
        .expect("cleanup must honor its own timeout")
        .expect("cleanup timeout must not hide successful semantics verification");

        assert_eq!(
            backend.object_count(),
            1,
            "the timed-out fake delete must leave the owned probe behind"
        );
    }

    #[tokio::test]
    async fn cached_verifier_runs_once_per_accessor_and_prefix() {
        let (op, backend) = conditional_test_operator(TestConditionalBehavior::CORRECT);

        ensure_conditional_write_semantics(&op, "/tenant/")
            .await
            .unwrap();
        let first_attempts = backend.write_attempts();
        ensure_conditional_write_semantics(&op, "tenant")
            .await
            .unwrap();

        assert_eq!(backend.write_attempts(), first_attempts);
        assert_eq!(backend.object_count(), 0);
    }

    #[test]
    fn test_build_operator_https_valid() {
        let cfg = StorageConfig {
            endpoint: "https://localhost:8333".to_string(),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            ..Default::default()
        };
        let op = build_operator(&cfg).expect("operator construction should succeed");
        assert!(
            op.info().full_capability().delete_with_version,
            "S3 operators must expose exact-version deletion for fail-closed chunk GC"
        );
    }

    #[test]
    fn test_build_operator_rejects_http_by_default() {
        let cfg = StorageConfig {
            endpoint: "http://localhost:8333".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            ..Default::default()
        };

        let err = build_operator(&cfg).unwrap_err();
        assert!(err.to_string().contains("plaintext HTTP"), "{err:#}");
    }

    #[test]
    fn endpoint_transport_errors_never_echo_credential_bearing_input() {
        for endpoint in [
            "not-a-url-with-MALFORMED-secret?token=MALFORMED-query",
            "http://plain-user:PLAIN-secret@plain.example.test:8333/PLAIN-path?token=PLAIN-query#PLAIN-fragment",
            "ftp://ftp-user:FTP-secret@ftp.example.test/FTP-path?token=FTP-query#FTP-fragment",
            "custom-secret://custom-user:CUSTOM-secret@custom.example.test/CUSTOM-path?token=CUSTOM-query#CUSTOM-fragment",
        ] {
            let cfg = StorageConfig {
                endpoint: endpoint.into(),
                access_key_id: "test-key".into(),
                secret_access_key: "test-secret".into(),
                ..Default::default()
            };

            let rendered = format!("{:#}", validate_endpoint_transport(&cfg).unwrap_err());
            for forbidden in [
                "MALFORMED-secret",
                "MALFORMED-query",
                "plain-user",
                "PLAIN-secret",
                "PLAIN-path",
                "PLAIN-query",
                "PLAIN-fragment",
                "ftp-user",
                "FTP-secret",
                "FTP-path",
                "FTP-query",
                "FTP-fragment",
                "custom-secret",
                "custom-user",
                "CUSTOM-secret",
                "CUSTOM-path",
                "CUSTOM-query",
                "CUSTOM-fragment",
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "transport error leaked {forbidden}: {rendered}"
                );
            }
        }
    }

    #[test]
    fn test_build_operator_allows_explicit_insecure_http() {
        let cfg = StorageConfig {
            endpoint: "http://localhost:8333".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            allow_insecure_http: true,
            ..Default::default()
        };

        assert!(build_operator(&cfg).is_ok());
    }

    #[test]
    fn test_build_operator_with_s3_http_controls() {
        let cfg = StorageConfig {
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            allow_insecure_http: false,
            s3_connect_timeout_secs: 5,
            s3_pool_idle_timeout_secs: 15,
            s3_pool_max_idle_per_host: 4,
            s3_http1_only: true,
            ca_cert_path: None,
        };

        assert!(
            build_s3_http_client(&cfg).unwrap().is_some(),
            "nonzero S3 HTTP controls should build a custom client"
        );
        assert!(
            build_operator_with_limits(&cfg, 4).is_ok(),
            "operator construction should succeed with S3 HTTP controls and concurrency limits"
        );
    }

    #[test]
    fn strict_redirect_policy_rejects_tls_downgrade() {
        assert!(redirect_scheme_allowed("https", false));
        assert!(!redirect_scheme_allowed("http", false));
        assert!(!redirect_scheme_allowed("file", false));
    }

    #[test]
    fn explicit_dev_opt_in_allows_http_redirects_only() {
        assert!(redirect_scheme_allowed("https", true));
        assert!(redirect_scheme_allowed("http", true));
        assert!(!redirect_scheme_allowed("file", true));
    }

    #[test]
    fn insecure_opt_in_warns_for_plaintext_and_https_downgrade_risk() {
        let plaintext = insecure_transport_warning("http", true).unwrap();
        let downgrade = insecure_transport_warning("https", true).unwrap();

        assert!(plaintext.contains("credentials are transmitted"));
        assert!(downgrade.contains("redirects may downgrade"));
        assert_ne!(plaintext, downgrade);
        assert!(insecure_transport_warning("https", false).is_none());
    }

    #[test]
    fn test_build_s3_http_client_reads_configured_ca_cert_path() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("missing-ca.pem");

        let cfg = StorageConfig {
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            ca_cert_path: Some(ca_path.clone()),
            ..Default::default()
        };

        let err = build_s3_http_client(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("reading S3 CA certificate"),
            "missing CA error should name the CA read failure: {err}"
        );
    }

    #[test]
    fn test_build_from_core_config_http_explicit_dev_opt_in() {
        // HTTP endpoint with explicit enforce_tls=false should succeed (and warn).
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "http://localhost:8333".into(),
            enforce_tls: false,
            ..Default::default()
        };
        let result = build_from_core_config(&storage, "key", "secret");
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_from_core_config_http_enforce_tls() {
        // HTTP endpoint with enforce_tls=true should fail
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "http://insecure:8333".into(),
            enforce_tls: true,
            ..Default::default()
        };
        let result = build_from_core_config(&storage, "key", "secret");
        assert!(result.is_err(), "HTTP + enforce_tls must fail");
        assert!(
            result.unwrap_err().to_string().contains("enforce_tls"),
            "error message should mention the plaintext transport"
        );
    }

    #[test]
    fn test_build_from_core_config_https() {
        // HTTPS endpoint with enforce_tls=true should succeed
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "https://s3.example.com:8333".into(),
            enforce_tls: true,
            ..Default::default()
        };
        let result = build_from_core_config(&storage, "key", "secret");
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_from_core_config_uses_ca_cert_path() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("missing-ca.pem");
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "https://s3.example.com:8333".into(),
            enforce_tls: true,
            ca_cert_path: Some(ca_path),
            ..Default::default()
        };

        let err = build_from_core_config(&storage, "key", "secret").unwrap_err();
        assert!(
            err.to_string().contains("reading S3 CA certificate"),
            "core config CA path should be passed to the operator: {err}"
        );
    }
}
