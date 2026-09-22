//! Narrow installed-evidence document reads backing the semantic cache.

use std::sync::Arc;
use std::time::Instant;

use rusqlite::OptionalExtension;
use sha2::Digest;

use super::common::sql_error;
use super::SqliteAuthorizationStore;
use crate::platform::auth::compiled_evidence::{CompiledInstalledEvidence, SemanticJobError};
use crate::platform::auth::AuthorizationStateError;
use crate::telemetry::{record_duration, DurationMetric, Outcome};

impl SqliteAuthorizationStore {
    /// Return the immutable compiled graph for an exact installed evidence
    /// document, reusing the store's bounded semantic cache.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizationStateError`] when the document is missing from
    /// the store or fails digest or semantic verification.
    #[tracing::instrument(
        name = "trellis.contract.compile_evidence",
        skip_all,
        fields(trellis.surface = "contract", trellis.operation = "compile_evidence")
    )]
    pub(in crate::platform::auth) async fn compiled_installed_evidence(
        &self,
        evidence_digest: &str,
    ) -> Result<Arc<CompiledInstalledEvidence>, AuthorizationStateError> {
        let total_started = Instant::now();
        let store = self.clone();
        let cpu = self.compiled_evidence.cpu_semaphore();
        let digest = evidence_digest.to_owned();
        let result = self
            .compiled_evidence
            .compiled_installed_evidence(evidence_digest, move || async move {
                let load_started = Instant::now();
                let loaded = store.load_evidence_document(&digest).await;
                record_duration(
                    DurationMetric::ContractAnalysis,
                    load_started.elapsed(),
                    "contract",
                    "compile_evidence",
                    "evidence_load",
                    if loaded.is_ok() {
                        Outcome::Ok
                    } else {
                        Outcome::Error
                    },
                );
                let (package_digest, evidence_json) = loaded.map_err(semantic_error)?;
                let cpu_started = Instant::now();
                let cpu_permit = Arc::clone(&cpu).acquire_owned().await;
                record_duration(
                    DurationMetric::ContractAnalysis,
                    cpu_started.elapsed(),
                    "contract",
                    "compile_evidence",
                    "cpu_wait",
                    if cpu_permit.is_ok() {
                        Outcome::Ok
                    } else {
                        Outcome::Error
                    },
                );
                let permit = cpu_permit.map_err(|_| {
                    SemanticJobError::Storage("semantic CPU pool closed".to_owned())
                })?;
                let expected_digest = digest.clone();
                let submitted = Instant::now();
                let (compiled, blocking_queue, compile) = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let blocking_queue = submitted.elapsed();
                    let compute_started = Instant::now();
                    let result =
                        compile_loaded_evidence(&expected_digest, &package_digest, &evidence_json);
                    (result, blocking_queue, compute_started.elapsed())
                })
                .await
                .map_err(|error| SemanticJobError::Worker(error.to_string()))?;
                record_duration(
                    DurationMetric::ContractAnalysis,
                    blocking_queue,
                    "contract",
                    "compile_evidence",
                    "blocking_queue",
                    Outcome::Ok,
                );
                record_duration(
                    DurationMetric::ContractAnalysis,
                    compile,
                    "contract",
                    "compile_evidence",
                    "compile",
                    if compiled.is_ok() {
                        Outcome::Ok
                    } else {
                        Outcome::Error
                    },
                );
                compiled
            })
            .await
            .map_err(SemanticJobError::into_state_error);
        record_duration(
            DurationMetric::ContractAnalysis,
            total_started.elapsed(),
            "contract",
            "compile_evidence",
            "total",
            if result.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Error
            },
        );
        result
    }

    /// Read the exact canonical evidence document and its claimed package
    /// digest for one installed evidence digest.
    async fn load_evidence_document(
        &self,
        evidence_digest: &str,
    ) -> Result<(String, String), AuthorizationStateError> {
        let evidence_digest = evidence_digest.to_owned();
        self.run_read(move |connection| {
            connection
                .query_row(
                    "SELECT package_digest, evidence_json FROM auth_package_evidence_documents
                     WHERE evidence_digest = ?1",
                    [&evidence_digest],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or(AuthorizationStateError::ParticipantMissing)
        })
        .await
    }
}

/// Verify one stored evidence document and compile its immutable graph.
fn compile_loaded_evidence(
    expected_digest: &str,
    package_digest: &str,
    evidence_json: &str,
) -> Result<CompiledInstalledEvidence, SemanticJobError> {
    let computed = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        sha2::Sha256::digest(evidence_json.as_bytes()),
    );
    if computed != expected_digest {
        return Err(SemanticJobError::InvalidEvidence(
            "package evidence document disagrees with its immutable digest".to_owned(),
        ));
    }
    let evidence: trellis_idl::PackageEvidence = serde_json::from_str(evidence_json)
        .map_err(|error| SemanticJobError::InvalidEvidence(error.to_string()))?;
    if evidence.root_digest != package_digest {
        return Err(SemanticJobError::InvalidEvidence(
            "package evidence document disagrees with its stored package digest".to_owned(),
        ));
    }
    let graph = trellis_idl::compile_evidence(evidence)
        .map_err(|error| SemanticJobError::InvalidEvidence(error.to_string()))?;
    if graph.root_digest() != package_digest {
        return Err(SemanticJobError::InvalidEvidence(
            "package evidence does not recompile to its stored semantic digest".to_owned(),
        ));
    }
    Ok(CompiledInstalledEvidence {
        evidence_digest: expected_digest.to_owned(),
        package_digest: package_digest.to_owned(),
        graph: Arc::new(graph),
    })
}

fn semantic_error(error: AuthorizationStateError) -> SemanticJobError {
    match error {
        AuthorizationStateError::ParticipantMissing => SemanticJobError::MissingDocument,
        AuthorizationStateError::InvalidRecord(message) => {
            SemanticJobError::InvalidEvidence(message)
        }
        AuthorizationStateError::Storage(message) => SemanticJobError::Storage(message),
        other => SemanticJobError::InvalidEvidence(other.to_string()),
    }
}
