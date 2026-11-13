//! Bounded self-update.
//!
//! This module owns *policy*: which release is acceptable, whether this
//! installation may be updated at all, and what the candidate must say about
//! itself. Everything mechanical is delegated to the published `eggup` crates,
//! which already own it:
//!
//! - acquisition and the transport seam: `eggup-acquisition` and
//!   `eggup-eggfetch`;
//! - the release manifest projection: `eggup-eggpack`;
//! - digest binding, bounded candidate execution, locked ownership
//!   revalidation, and rollback: `eggup-core`.
//!
//! Nothing here re-implements a checksum, a staging directory, a lock, or a
//! rollback. Adding a second copy of any of those is the failure mode this
//! module exists to avoid.
//!
//! # The release input
//!
//! A release publishes a `release-manifest.json` produced by `eggpack`, beside
//! the binaries it names. The manifest is the whole input: it carries the
//! release id, the asset name for each target, the asset's exact size, and
//! its SHA-256. The digest is *integrity* evidence, not authenticity — it
//! proves the bytes were not corrupted, not that they came from us.
//!
//! # The transport
//!
//! `eggup-eggfetch` is the only production transport. It carries its own
//! TLS stack, and that is the point: the release host rejects a generic
//! user agent, and a transport that cannot reach the version authority is not
//! qualified, however small it is.
//!
//! # Ownership
//!
//! The file about to be replaced must prove it is the file this process is
//! running. That proof is its exact SHA-256, re-read by `eggup` under the
//! mutation lock immediately before the commit. The file name, the `PATH`
//! order, and the directory it lives in prove nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use eggup_acquisition::{
    AcquisitionError, AcquisitionRequest, AcquisitionTransport, CancelFlag, FetchLimits,
    FetchOutcome,
};
use eggup_core::{
    AbsentPolicy, ArtifactMember, ArtifactSet, CommitOwnership, ExactIdentityValidator,
    InstallPlan, IntegrityRequirement, MemberId, Ownership, OwnershipVerifier, PermissionsIntent,
    ProductId, ReleaseId, TransactionDisposition, TransactionReceipt,
};
use eggup_eggpack::ManifestProjection;

use crate::config::UpdateSettings;

/// The product identity, which is also the release product id.
pub const PRODUCT: &str = "eggpass";

/// The crate whose published version is this build's version.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The exact byte ceiling for the release manifest document.
///
/// The real document is a few kilobytes. 1 MiB matches the manifest schema's
/// own ceiling and is still finitely bounded.
const MAX_METADATA_BYTES: usize = eggup_eggpack::MAX_MANIFEST_BYTES;

/// The exact byte ceiling for one release artifact.
const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

/// How many directories above the executable are searched for a Cargo
/// installation root.
const CARGO_ROOT_SEARCH_DEPTH: usize = 4;

/// The User-Agent every outbound request identifies itself with.
///
/// The release host answers 403 to an anonymous or generic agent, so this is a
/// hard requirement of the version authority, not politeness.
const USER_AGENT: &str = concat!(
    "eggpass/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/eggstack/eggpass)"
);

/// The contracted triple for the host this binary is running on, if published.
///
/// A host with no published artifact has nothing to update to, which is a
/// statement rather than an error.
pub fn host_target() -> Option<&'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some("x86_64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some("aarch64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("x86_64-apple-darwin")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("aarch64-apple-darwin")
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("x86_64-pc-windows-msvc")
    }
    // Any other host has no published artifact.
}

/// Why this installation cannot be updated, in terms a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// The running executable is positively recorded in a Cargo installation
    /// root's package metadata. Cargo owns that file and its bookkeeping;
    /// replacing it here would leave `cargo install --list` and uninstall
    /// lying. The correct command is printed instead.
    CargoManaged {
        /// The installation root Cargo owns.
        root: PathBuf,
        /// The version Cargo recorded.
        version: String,
    },
    /// The running executable is not Cargo-managed and is positively identified
    /// by its exact digest, which is revalidated under `eggup`'s mutation lock
    /// immediately before replacement.
    SelfManaged {
        /// The digest of the running executable.
        digest: [u8; 32],
    },
    /// The running executable could not be positively identified, so
    /// replacement cannot be proven safe.
    Unprovable {
        /// What went wrong.
        detail: String,
    },
}

impl Provenance {
    /// A short, stable, machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Provenance::CargoManaged { .. } => "cargo_managed",
            Provenance::SelfManaged { .. } => "self_managed",
            Provenance::Unprovable { .. } => "unprovable_ownership",
        }
    }

    /// The exact command the user should run instead, when there is one.
    pub fn remediation(&self) -> Option<String> {
        match self {
            Provenance::CargoManaged { root, .. } => Some(format!(
                "cargo install {PRODUCT} --locked --force --root {}",
                root.display()
            )),
            Provenance::Unprovable { .. } => Some(format!(
                "Reinstall with the published installer, then retry: \
                 https://github.com/eggstack/{PRODUCT}"
            )),
            Provenance::SelfManaged { .. } => None,
        }
    }
}

/// A verifier that proves ownership of the live destination by exact prior
/// SHA-256 content.
///
/// This is the whole ownership proof for a self-update: the file being replaced
/// must hash to the digest observed before the transaction began, re-read by
/// `eggup` under the mutation lock.
#[derive(Debug, Clone)]
struct LiveDigestVerifier {
    expected: HashMap<MemberId, [u8; 32]>,
}

impl OwnershipVerifier for LiveDigestVerifier {
    fn verify(&self, member: &MemberId, destination: &Path) -> Ownership {
        let Some(expected) = self.expected.get(member) else {
            return Ownership::Unknown;
        };
        match std::fs::symlink_metadata(destination) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ownership::Absent,
            Err(_) => Ownership::Unknown,
            Ok(meta) if meta.file_type().is_symlink() => Ownership::Foreign,
            Ok(meta) if !meta.is_file() => Ownership::Foreign,
            Ok(_) => match eggup_core::hash_file(destination) {
                Ok(actual) if &actual == expected => Ownership::Owned,
                Ok(_) => Ownership::Foreign,
                Err(_) => Ownership::Unknown,
            },
        }
    }
}

/// What the update would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    /// The version running now.
    pub from_version: String,
    /// The version the catalogue names.
    pub to_version: String,
    /// The target triple the asset is for.
    pub target: String,
    /// The asset that will be installed.
    pub asset: String,
    /// The release the asset comes from.
    pub release: String,
    /// How this installation is owned.
    pub provenance: Provenance,
}

/// Why an update did not happen.
#[derive(Debug)]
pub enum UpdateError {
    /// The installation may not be updated by this command.
    Provenance {
        /// What stopped it.
        provenance: Provenance,
    },
    /// The host has no published artifact.
    HostUnsupported {
        /// What was missing.
        detail: String,
    },
    /// The catalogue could not be read.
    Catalogue {
        /// What went wrong.
        detail: String,
    },
    /// The release named is not the one asked for.
    WrongRelease {
        /// The version asked for.
        wanted: String,
        /// The version the catalogue names.
        published: String,
    },
    /// The running build is already the published one.
    AlreadyCurrent {
        /// The running version.
        current: String,
        /// The published version.
        published: String,
    },
    /// The running build is newer than the published one.
    NewerThanPublished {
        /// The running version.
        current: String,
        /// The published version.
        published: String,
    },
    /// The release could not be fetched or was not usable.
    ReleaseEvidence {
        /// What went wrong.
        detail: String,
    },
    /// The update transaction failed.
    Transaction {
        /// What went wrong.
        detail: String,
    },
    /// The transaction reached a terminal state that is not a commit.
    Receipt {
        /// The disposition.
        disposition: &'static str,
        /// The recorded detail.
        detail: String,
        /// Where the retained backup is, when there is one.
        recovery_path: Option<PathBuf>,
    },
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Provenance { provenance } => {
                write!(f, "this installation cannot be updated in place")?;
                if let Some(remediation) = provenance.remediation() {
                    write!(f, "; run `{remediation}` instead")?;
                }
                Ok(())
            }
            UpdateError::HostUnsupported { detail } => write!(f, "{detail}"),
            UpdateError::Catalogue { detail } => write!(f, "{detail}"),
            UpdateError::WrongRelease { wanted, published } => write!(
                f,
                "the catalogue names release {published}, not the requested {wanted}"
            ),
            UpdateError::AlreadyCurrent { current, published } => write!(
                f,
                "eggpass {current} is already the published version ({published})"
            ),
            UpdateError::NewerThanPublished { current, published } => write!(
                f,
                "eggpass {current} is newer than the published version ({published}); \
                 refusing to downgrade"
            ),
            UpdateError::ReleaseEvidence { detail } => write!(f, "{detail}"),
            UpdateError::Transaction { detail } => write!(f, "{detail}"),
            UpdateError::Receipt {
                disposition,
                detail,
                recovery_path,
            } => {
                write!(f, "the update {disposition}: {detail}")?;
                if let Some(path) = recovery_path {
                    write!(f, "\nThe previous binary is at {}", path.display())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for UpdateError {}

/// Run the update.
///
/// `version` names an exact release; without it the latest published release
/// is used. `check_only` reports what would happen and changes nothing.
pub fn update(
    settings: &UpdateSettings,
    version: Option<&str>,
    check_only: bool,
) -> Result<UpdatePlan, UpdateError> {
    let current =
        eggup_core::CurrentExecutable::resolve().map_err(|err| UpdateError::Transaction {
            detail: format!("could not resolve the running executable: {err}"),
        })?;

    // The canonical path is the real file. A symlink would be replaced by the
    // transaction, and the ownership proof is about the bytes that are there.
    let current_exe = current.target().to_path_buf();

    let provenance = classify_provenance(&current_exe);
    let target = host_target().ok_or_else(|| UpdateError::HostUnsupported {
        detail: format!(
            "there is no published eggpass build for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    })?;

    // A mutating update refuses forbidden provenance *before* asking the
    // release host anything. When local provenance alone makes the mutation
    // impossible, there is nothing the network can tell us that changes the
    // answer.
    if !check_only && !matches!(provenance, Provenance::SelfManaged { .. }) {
        return Err(UpdateError::Provenance { provenance });
    }

    let base = release_base_url(settings, version);
    let manifest_url = format!("{base}/{}", settings.manifest_name);

    let manifest = fetch_metadata(&manifest_url)?;
    let projection =
        eggup_eggpack::project_json(&manifest, target).map_err(|err| UpdateError::Catalogue {
            detail: format!("the release manifest could not be used: {err}"),
        })?;

    let (published, artifacts) = match &projection {
        ManifestProjection::Installable {
            release, artifacts, ..
        } => (release.as_str().to_string(), artifacts.clone()),
        ManifestProjection::Archive { .. } => {
            return Err(UpdateError::Catalogue {
                detail: "this release is an archive, and eggpass publishes a single \
                         binary per target; use the published installer instead"
                    .to_string(),
            });
        }
    };

    if let Some(wanted) = version
        && wanted.trim_start_matches('v') != published
    {
        return Err(UpdateError::WrongRelease {
            wanted: wanted.to_string(),
            published: published.clone(),
        });
    }

    match compare_release_versions(&published, CURRENT_VERSION) {
        Some(std::cmp::Ordering::Equal) => {
            return Err(UpdateError::AlreadyCurrent {
                current: CURRENT_VERSION.to_string(),
                published,
            });
        }
        Some(std::cmp::Ordering::Less) => {
            // Never downgrade. A build that is newer than the published release
            // is a local or unreleased build, and the catalogue is not
            // entitled to replace it.
            return Err(UpdateError::NewerThanPublished {
                current: CURRENT_VERSION.to_string(),
                published,
            });
        }
        Some(std::cmp::Ordering::Greater) => {}
        // The published side is always a plain release, so the side that
        // cannot be ordered is this build.
        None => {
            return Err(UpdateError::Transaction {
                detail: format!(
                    "the running version {CURRENT_VERSION} is not a plain release, so it \
                     cannot be compared with the published one"
                ),
            });
        }
    }

    // The binary is the one member that matters. The manifest may name more.
    let binary = artifacts
        .iter()
        .find(|artifact| artifact.destination == PRODUCT)
        .or_else(|| artifacts.first())
        .ok_or_else(|| UpdateError::Catalogue {
            detail: "the release manifest names no artifacts".to_string(),
        })?;

    let plan = UpdatePlan {
        from_version: CURRENT_VERSION.to_string(),
        to_version: published.clone(),
        target: target.to_string(),
        asset: binary.artifact_name.clone(),
        release: published,
        provenance,
    };

    if check_only {
        return Ok(plan);
    }

    execute(settings, &plan, binary, &current_exe)
}

/// The base URL a release's assets are served from.
fn release_base_url(settings: &UpdateSettings, version: Option<&str>) -> String {
    let repository = settings.repository.trim_matches('/');
    match version {
        Some(version) => format!(
            "https://github.com/{repository}/releases/download/v{}",
            version.trim_start_matches('v')
        ),
        None => format!("https://github.com/{repository}/releases/latest/download"),
    }
}

/// Fetch a small document into memory.
fn fetch_metadata(url: &str) -> Result<Vec<u8>, UpdateError> {
    let transport = transport()?;
    let request = AcquisitionRequest::new(url).map_err(|err| UpdateError::Catalogue {
        detail: format!("the release manifest request is not usable: {err}"),
    })?;

    let mut limits = limits();
    limits.max_metadata_bytes = MAX_METADATA_BYTES;

    let cancel = CancelFlag::new();
    let outcome = transport
        .fetch_metadata(&request, limits, &cancel)
        .map_err(|err| UpdateError::Catalogue {
            detail: format!("the release manifest could not be read: {}", describe(&err)),
        })?;

    match outcome {
        FetchOutcome::Success(body) => Ok(body.bytes().to_vec()),
        FetchOutcome::NotFound => Err(UpdateError::Catalogue {
            detail: format!("the release host reports {url} is absent"),
        }),
    }
}

/// Download one artifact to `destination`.
fn fetch_artifact(url: &str, destination: &Path) -> Result<u64, UpdateError> {
    let transport = transport()?;
    let request = AcquisitionRequest::new(url).map_err(|err| UpdateError::ReleaseEvidence {
        detail: format!("the release request is not usable: {err}"),
    })?;

    let cancel = CancelFlag::new();
    let outcome = transport
        .fetch_artifact(&request, destination, limits(), &cancel)
        .map_err(|err| UpdateError::ReleaseEvidence {
            detail: format!("the release could not be downloaded: {}", describe(&err)),
        })?;

    match outcome {
        FetchOutcome::Success(evidence) => Ok(evidence.bytes_written()),
        FetchOutcome::NotFound => Err(UpdateError::ReleaseEvidence {
            detail: format!("the release host reports {url} is absent"),
        }),
    }
}

/// The transport every request goes out through.
fn transport() -> Result<eggup_eggfetch::EggfetchTransport, UpdateError> {
    let config = eggup_eggfetch::EggfetchConfig::strict()
        .user_agent(USER_AGENT)
        .timeouts(CONNECT_TIMEOUT, TOTAL_TIMEOUT);
    eggup_eggfetch::EggfetchTransport::strict(config).map_err(|err| UpdateError::Catalogue {
        detail: format!("the release transport is unavailable: {}", describe(&err)),
    })
}

/// The bounds every fetch works within.
fn limits() -> FetchLimits {
    FetchLimits {
        max_metadata_bytes: MAX_METADATA_BYTES,
        max_artifact_bytes: MAX_ARTIFACT_BYTES,
        connect_timeout: CONNECT_TIMEOUT,
        total_timeout: TOTAL_TIMEOUT,
    }
}

/// A bounded description of an acquisition failure.
fn describe(error: &AcquisitionError) -> String {
    format!("{error:?}")
}

/// Run the transaction: download, verify, validate, commit.
fn execute(
    settings: &UpdateSettings,
    plan: &UpdatePlan,
    binary: &eggup_eggpack::ArtifactRequirement,
    current_exe: &Path,
) -> Result<UpdatePlan, UpdateError> {
    let live_digest = match &plan.provenance {
        Provenance::SelfManaged { digest } => *digest,
        other => {
            return Err(UpdateError::Provenance {
                provenance: other.clone(),
            });
        }
    };

    let staging = staging_dir()?;
    let outcome = (|| -> Result<TransactionReceipt, UpdateError> {
        let base = release_base_url(settings, Some(&plan.release));
        let acquired = staging.join(&binary.artifact_name);

        let written = fetch_artifact(&format!("{base}/{}", binary.artifact_name), &acquired)?;
        if written == 0 {
            return Err(UpdateError::ReleaseEvidence {
                detail: format!("the release asset {} is empty", binary.artifact_name),
            });
        }
        if written != binary.exact_size {
            return Err(UpdateError::ReleaseEvidence {
                detail: format!(
                    "the release asset {} is {written} bytes, but the manifest says {}",
                    binary.artifact_name, binary.exact_size
                ),
            });
        }

        let install_root = current_exe
            .parent()
            .ok_or_else(|| UpdateError::Transaction {
                detail: "the running executable has no parent directory".to_string(),
            })?
            .to_path_buf();

        let file_name = current_exe
            .file_name()
            .ok_or_else(|| UpdateError::Transaction {
                detail: "the running executable has no file name".to_string(),
            })?
            .to_owned();

        let member = ArtifactMember::new(binary.member_id.clone(), acquired, file_name)
            .map_err(|err| UpdateError::Transaction {
                detail: format!("could not declare the update member: {err}"),
            })?
            // The candidate must be executable to prove its own identity, and
            // must stay executable once committed.
            .with_permissions(PermissionsIntent::Executable)
            .with_integrity(IntegrityRequirement::Sha256(binary.sha256));

        let artifacts = ArtifactSet::single(member).map_err(|err| UpdateError::Transaction {
            detail: format!("could not assemble the update set: {err}"),
        })?;

        let install_plan = InstallPlan::new(
            ProductId::new(PRODUCT).map_err(|err| UpdateError::Transaction {
                detail: format!("invalid product identity: {err}"),
            })?,
            ReleaseId::new(plan.release.clone()).map_err(|err| UpdateError::Transaction {
                detail: format!("invalid release identity: {err}"),
            })?,
            install_root,
            artifacts,
        )
        .map_err(|err| UpdateError::Transaction {
            detail: format!("the install plan is invalid: {err}"),
        })?;

        let prepared = install_plan
            .prepare()
            .map_err(|err| UpdateError::Transaction {
                detail: format!("could not stage the update: {err}"),
            })?;
        let verified = prepared
            .verify_integrity()
            .map_err(|err| UpdateError::Transaction {
                detail: format!("release integrity verification failed: {err}"),
            })?;

        // The candidate must identify itself as exactly the version the
        // manifest named. This runs the acquired bytes, so it is the only
        // check that the thing about to replace this process is really us.
        let validator =
            ExactIdentityValidator::new(binary.member_id.clone(), expected_identity(&plan.release))
                .args(["--version"]);
        let validated = verified
            .validate(&validator)
            .map_err(|err| UpdateError::Transaction {
                detail: format!(
                    "the candidate did not identify itself as {PRODUCT} {}\n{err}",
                    plan.release
                ),
            })?;

        let mut expected = HashMap::new();
        expected.insert(binary.member_id.clone(), live_digest);
        let verifier = LiveDigestVerifier { expected };

        validated
            .commit(CommitOwnership::new(&verifier, AbsentPolicy::DenyCreate))
            .map_err(|err| UpdateError::Transaction {
                detail: format!("the commit failed: {err}"),
            })
    })();

    // Cleanup runs on both the success and the failure path, so it is captured
    // rather than propagated: the transaction's own outcome is the more
    // important fact and must not be masked by a leftover temporary directory.
    let cleanup = std::fs::remove_dir_all(&staging)
        .err()
        .map(|err| err.to_string());

    let receipt = match outcome {
        Ok(receipt) => receipt,
        Err(err) => {
            if let Some(detail) = cleanup {
                eprintln!(
                    "eggpass: the staging directory {} could not be removed: {detail}",
                    staging.display()
                );
            }
            return Err(err);
        }
    };

    let disposition = receipt.disposition();
    if let Some(detail) = cleanup {
        let claim = disposition_claim(disposition);
        return Err(UpdateError::Transaction {
            detail: format!(
                "the update transaction {claim}, but its staging directory {} could not \
                 be removed: {detail}",
                staging.display()
            ),
        });
    }

    match disposition {
        TransactionDisposition::Committed => Ok(plan.clone()),
        TransactionDisposition::RolledBack => Err(UpdateError::Receipt {
            disposition: "was rolled back",
            detail: receipt
                .failure()
                .map(|failure| failure.detail().to_owned())
                .unwrap_or_else(|| "no failure detail was recorded".to_string()),
            recovery_path: receipt.recovery_path().map(Path::to_path_buf),
        }),
        TransactionDisposition::RecoveryRequired => Err(UpdateError::Receipt {
            disposition: "needs recovery",
            detail: receipt
                .failure()
                .map(|failure| failure.detail().to_owned())
                .unwrap_or_else(|| "no failure detail was recorded".to_string()),
            recovery_path: receipt.recovery_path().map(Path::to_path_buf),
        }),
    }
}

/// What a disposition means in a sentence.
fn disposition_claim(disposition: TransactionDisposition) -> &'static str {
    match disposition {
        TransactionDisposition::Committed => "committed",
        TransactionDisposition::RolledBack => {
            "was rolled back, so the previous binary was restored"
        }
        TransactionDisposition::RecoveryRequired => {
            "needs recovery, so the previous binary may not be in place"
        }
    }
}

/// The exact identity string a release candidate must print.
fn expected_identity(version: &str) -> String {
    format!("{PRODUCT} {version}\n")
}

/// Create a private staging directory for this run.
///
/// The name is collision-resistant and the directory is created *fresh* — not
/// with `create_dir_all`, which succeeds on an existing path — so one
/// invocation never stages bytes into another's tree.
fn staging_dir() -> Result<PathBuf, UpdateError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let unique =
        std::env::temp_dir().join(format!("{PRODUCT}-update-{}-{nanos}", std::process::id()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&unique)
            .map_err(|err| UpdateError::Transaction {
                detail: format!(
                    "could not create a private staging directory at {}: {err}",
                    unique.display()
                ),
            })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(&unique).map_err(|err| UpdateError::Transaction {
            detail: format!(
                "could not create a private staging directory at {}: {err}",
                unique.display()
            ),
        })?;
    }

    Ok(unique)
}

/// Decide whether this installation may be updated in place.
pub fn classify_provenance(current_exe: &Path) -> Provenance {
    // A Cargo installation root owns the file when its own record says so.
    for root in cargo_roots_above(current_exe) {
        match read_cargo_record(&root) {
            CargoRecord::Recorded { version } => {
                return Provenance::CargoManaged { root, version };
            }
            CargoRecord::Uninterpretable { detail } => {
                // The record is there but could not be read. That is positive
                // evidence that this directory is manager-sensitive, and the
                // content that would settle ownership is exactly what could
                // not be read. Failing open here would replace the file and
                // leave Cargo's bookkeeping lying.
                return Provenance::Unprovable { detail };
            }
            CargoRecord::Absent | CargoRecord::DoesNotClaim => {}
        }
    }

    match eggup_core::hash_file(current_exe) {
        Ok(digest) => Provenance::SelfManaged { digest },
        Err(err) => Provenance::Unprovable {
            detail: format!(
                "the running executable could not be hashed, so ownership cannot be \
                 proven: {err}"
            ),
        },
    }
}

/// What a Cargo installation root's own record says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CargoRecord {
    /// Cargo positively records this product in this root.
    Recorded { version: String },
    /// The record is readable and does not claim this product.
    DoesNotClaim,
    /// No record file at this root.
    Absent,
    /// A record exists but could not be interpreted.
    Uninterpretable { detail: String },
}

/// Cargo installation roots that could own `path`, nearest first.
///
/// Cargo writes `ROOT/bin/<exe>` plus `ROOT/.crates.toml`, so each candidate is
/// the parent of some ancestor `bin` directory. This is a *location* test only;
/// it never grants ownership.
fn cargo_roots_above(path: &Path) -> Vec<PathBuf> {
    let Some(bin_dir) = path.parent() else {
        return Vec::new();
    };
    bin_dir
        .ancestors()
        .take(CARGO_ROOT_SEARCH_DEPTH + 1)
        .filter_map(Path::parent)
        .map(Path::to_path_buf)
        .collect()
}

/// Read a Cargo installation root's `.crates.toml` and report what it says.
fn read_cargo_record(root: &Path) -> CargoRecord {
    let path = root.join(".crates.toml");

    let metadata = match std::fs::metadata(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return CargoRecord::Absent,
        Err(err) => {
            return CargoRecord::Uninterpretable {
                detail: format!(
                    "the Cargo record {} exists but could not be inspected: {err}",
                    path.display()
                ),
            };
        }
        Ok(metadata) => metadata,
    };

    if !metadata.is_file() {
        return CargoRecord::Uninterpretable {
            detail: format!("the Cargo record {} is not a regular file", path.display()),
        };
    }
    if metadata.len() > MAX_METADATA_BYTES as u64 {
        return CargoRecord::Uninterpretable {
            detail: format!(
                "the Cargo record {} is {} bytes, above the {MAX_METADATA_BYTES} byte ceiling",
                path.display(),
                metadata.len()
            ),
        };
    }

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            return CargoRecord::Uninterpretable {
                detail: format!(
                    "the Cargo record {} could not be read: {err}",
                    path.display()
                ),
            };
        }
    };

    let value: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            return CargoRecord::Uninterpretable {
                detail: format!(
                    "the Cargo record {} is not valid TOML: {err}",
                    path.display()
                ),
            };
        }
    };

    let Some(table) = value.get("v1").and_then(toml::Value::as_table) else {
        return CargoRecord::Uninterpretable {
            detail: format!(
                "the Cargo record {} has no `v1` table, which is not a supported Cargo \
                 record shape",
                path.display()
            ),
        };
    };

    for (key, binaries) in table {
        // The key is `<name> <version> (<source>)`.
        let Some(rest) = key.split_whitespace().next() else {
            continue;
        };
        if rest != PRODUCT {
            continue;
        }
        let version = key
            .split_whitespace()
            .nth(1)
            .unwrap_or("unknown")
            .to_string();
        let claims = binaries
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(toml::Value::as_str)
                    .any(|name| name == PRODUCT)
            })
            .unwrap_or(false);
        if claims {
            return CargoRecord::Recorded { version };
        }
    }

    CargoRecord::DoesNotClaim
}

/// Compare two plain `X.Y.Z` versions.
///
/// `None` when either side is not a plain release, so a caller can never
/// accidentally order a prerelease or a malformed string.
pub fn compare_release_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let parse = |value: &str| -> Option<[u64; 3]> {
        if !is_release_version(value) {
            return None;
        }
        let mut parts = value.split('.');
        let mut out = [0u64; 3];
        for slot in out.iter_mut() {
            *slot = parts.next()?.parse().ok()?;
        }
        Some(out)
    };
    Some(parse(left)?.cmp(&parse(right)?))
}

/// A plain `X.Y.Z` release with no prerelease or build suffix.
pub fn is_release_version(version: &str) -> bool {
    let mut parts = version.split('.');
    for _ in 0..3 {
        match parts.next() {
            Some(part) if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_release_is_recognised() {
        assert!(is_release_version("0.1.0"));
        assert!(is_release_version("10.20.30"));
        assert!(!is_release_version("0.1"));
        assert!(!is_release_version("0.1.0.1"));
        assert!(!is_release_version("0.1.0-rc1"));
        assert!(!is_release_version("0.1.0+build"));
        assert!(!is_release_version(""));
    }

    #[test]
    fn versions_are_ordered_numerically() {
        assert_eq!(
            compare_release_versions("0.2.0", "0.1.9"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            compare_release_versions("0.1.0", "0.1.0"),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(
            compare_release_versions("0.1.0", "0.2.0"),
            Some(std::cmp::Ordering::Less)
        );
        // Ten is later than nine, which a string comparison gets wrong.
        assert_eq!(
            compare_release_versions("0.10.0", "0.9.0"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(compare_release_versions("0.1", "0.1.0"), None);
    }

    #[test]
    fn the_release_urls_follow_the_repository() {
        let settings = UpdateSettings::default();
        assert_eq!(
            release_base_url(&settings, None),
            "https://github.com/eggstack/eggpass/releases/latest/download"
        );
        assert_eq!(
            release_base_url(&settings, Some("0.2.0")),
            "https://github.com/eggstack/eggpass/releases/download/v0.2.0"
        );
        assert_eq!(
            release_base_url(&settings, Some("v0.2.0")),
            "https://github.com/eggstack/eggpass/releases/download/v0.2.0"
        );
    }

    #[test]
    fn the_identity_string_is_exact() {
        assert_eq!(expected_identity("0.2.0"), "eggpass 0.2.0\n");
    }

    #[test]
    fn the_host_target_is_the_canonical_triple() {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(host_target(), Some("aarch64-apple-darwin"));
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(host_target(), Some("x86_64-unknown-linux-gnu"));
    }
}
