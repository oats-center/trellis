PRAGMA foreign_keys = ON;

-- Fresh platform initialization; authorization policy and OAuth state share
-- this store and its ordinary transaction/idempotency infrastructure.

CREATE TABLE trellis_platform_store_marker (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    jetstream_id TEXT NOT NULL CHECK (length(jetstream_id) = 32),
    jetstream_bound INTEGER NOT NULL DEFAULT 0 CHECK (jetstream_bound IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO trellis_platform_store_marker (id, jetstream_id)
VALUES (1, lower(hex(randomblob(16))));

CREATE TABLE auth_principals (
    principal_id TEXT PRIMARY KEY CHECK (length(principal_id) > 0),
    kind TEXT NOT NULL CHECK (kind IN ('user', 'service', 'device')),
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled', 'revoked')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    disabled_at INTEGER CHECK (disabled_at BETWEEN 0 AND 9007199254740991),
    revoked_at INTEGER CHECK (revoked_at BETWEEN 0 AND 9007199254740991),
    CHECK ((state = 'disabled') = (disabled_at IS NOT NULL)),
    CHECK ((state = 'revoked') = (revoked_at IS NOT NULL))
);

CREATE TABLE auth_provider_identities (
    provider TEXT NOT NULL CHECK (length(provider) > 0),
    provider_subject TEXT NOT NULL CHECK (length(provider_subject) > 0),
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    linked_at INTEGER NOT NULL CHECK (linked_at BETWEEN 0 AND 9007199254740991),
    last_seen_at INTEGER NOT NULL CHECK (last_seen_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY (provider, provider_subject)
);

CREATE TABLE auth_package_evidence (
    package_digest TEXT PRIMARY KEY CHECK (length(package_digest) = 43),
    platform_trusted INTEGER NOT NULL DEFAULT 0 CHECK (platform_trusted IN (0, 1)),
    accepted_at INTEGER NOT NULL CHECK (accepted_at BETWEEN 0 AND 9007199254740991),
    trusted_at INTEGER CHECK (trusted_at BETWEEN 0 AND 9007199254740991),
    trusted_by TEXT,
    CHECK ((trusted_at IS NULL) = (trusted_by IS NULL)),
    CHECK (platform_trusted = 1 OR trusted_at IS NULL)
);
CREATE TRIGGER auth_package_evidence_body_is_immutable
BEFORE UPDATE OF package_digest, accepted_at ON auth_package_evidence
BEGIN
    SELECT RAISE(ABORT, 'accepted package evidence is immutable');
END;
CREATE TRIGGER auth_package_evidence_trust_is_monotonic
BEFORE UPDATE OF platform_trusted, trusted_at, trusted_by ON auth_package_evidence
WHEN OLD.platform_trusted = 1 OR NEW.platform_trusted != 1
BEGIN
    SELECT RAISE(ABORT, 'accepted package trust cannot be removed or rewritten');
END;
CREATE TRIGGER auth_package_evidence_no_delete
BEFORE DELETE ON auth_package_evidence
BEGIN
    SELECT RAISE(ABORT, 'accepted package evidence is retained');
END;

CREATE TABLE auth_package_evidence_documents (
    evidence_digest TEXT PRIMARY KEY CHECK (length(evidence_digest) = 43),
    package_digest TEXT NOT NULL REFERENCES auth_package_evidence(package_digest),
    evidence_json TEXT NOT NULL CHECK (json_valid(evidence_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991)
);
CREATE INDEX auth_package_evidence_documents_package_digest
ON auth_package_evidence_documents(package_digest);
CREATE TRIGGER auth_package_evidence_document_is_immutable
BEFORE UPDATE ON auth_package_evidence_documents
BEGIN
    SELECT RAISE(ABORT, 'accepted package evidence document is immutable');
END;
CREATE TRIGGER auth_package_evidence_document_no_delete
BEFORE DELETE ON auth_package_evidence_documents
BEGIN
    SELECT RAISE(ABORT, 'accepted package evidence document is retained');
END;

CREATE TABLE auth_installed_participants (
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
    participant_kind TEXT NOT NULL CHECK (participant_kind IN ('service', 'device')),
    participant_digest TEXT NOT NULL CHECK (length(participant_digest) = 43),
    needs_digest TEXT NOT NULL CHECK (length(needs_digest) = 43),
    package_digest TEXT NOT NULL REFERENCES auth_package_evidence(package_digest),
    evidence_digest TEXT NOT NULL REFERENCES auth_package_evidence_documents(evidence_digest),
    participant_path TEXT NOT NULL CHECK (length(participant_path) > 0),
    companion_request_id TEXT CHECK (companion_request_id IS NULL OR length(companion_request_id) > 0),
    companion_request_kind TEXT CHECK (companion_request_kind IN ('browser', 'native')),
    companion_required INTEGER NOT NULL CHECK (companion_required IN (0, 1)),
    projection_json TEXT NOT NULL CHECK (json_valid(projection_json)),
    installed_at INTEGER NOT NULL CHECK (installed_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY (participant_id, revision),
    CHECK ((companion_request_id IS NULL) = (companion_request_kind IS NULL)),
    CHECK (companion_required = 0 OR companion_request_id IS NOT NULL)
);
CREATE TRIGGER auth_installed_participant_is_immutable
BEFORE UPDATE ON auth_installed_participants
BEGIN
    SELECT RAISE(ABORT, 'installed participant snapshots are immutable');
END;

CREATE TRIGGER auth_installed_participant_no_delete
BEFORE DELETE ON auth_installed_participants
BEGIN
    SELECT RAISE(ABORT, 'installed participant snapshots are retained');
END;








CREATE TABLE auth_deployments (
    deployment_id TEXT PRIMARY KEY CHECK (length(deployment_id) > 0),
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    participant_kind TEXT NOT NULL CHECK (participant_kind IN ('service', 'device')),
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled', 'revoked')),
    expires_at INTEGER CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    UNIQUE (deployment_id, participant_id)
);

CREATE TABLE auth_instances (
    instance_id TEXT PRIMARY KEY CHECK (length(instance_id) > 0),
    deployment_id TEXT NOT NULL REFERENCES auth_deployments(deployment_id) ON DELETE CASCADE,
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    -- Exact participant revision this instance's software is actually running.
    installed_revision INTEGER NOT NULL CHECK (installed_revision >= 1),
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled', 'revoked', 'stale')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    UNIQUE (instance_id, deployment_id)
);

CREATE TABLE auth_devices (
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    deployment_id TEXT NOT NULL REFERENCES auth_deployments(deployment_id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('pending', 'active', 'disabled', 'revoked')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    PRIMARY KEY (principal_id, deployment_id)
);


CREATE TABLE auth_resources (
    resource_id TEXT PRIMARY KEY CHECK (length(resource_id) = 43),
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('user', 'deployment')),
    owner_id TEXT NOT NULL CHECK (length(owner_id) > 0),
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    kind TEXT NOT NULL CHECK (kind IN ('consumer', 'job', 'kv', 'state', 'store')),
    local_name TEXT NOT NULL CHECK (length(local_name) > 0),
    commitment_json TEXT NOT NULL CHECK (json_valid(commitment_json)),
    physical_id TEXT CHECK (physical_id IS NULL OR length(physical_id) > 0),
    actual_json TEXT CHECK (actual_json IS NULL OR json_valid(actual_json)),
    state TEXT NOT NULL CHECK (state IN ('detached', 'destroying', 'failed', 'pending', 'ready')),
    readiness_reason TEXT,
    binding_revision INTEGER NOT NULL CHECK (binding_revision BETWEEN 1 AND 9007199254740991),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    UNIQUE (owner_kind, owner_id, participant_id, kind, local_name),
    CHECK (updated_at >= created_at)
);

CREATE TABLE auth_resource_history (
    resource_id TEXT NOT NULL REFERENCES auth_resources(resource_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
    commitment_json TEXT NOT NULL CHECK (json_valid(commitment_json)),
    actual_json TEXT CHECK (actual_json IS NULL OR json_valid(actual_json)),
    state TEXT NOT NULL CHECK (state IN ('detached', 'destroying', 'failed', 'pending', 'ready')),
    changed_at INTEGER NOT NULL CHECK (changed_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY (resource_id, revision)
);

CREATE TABLE auth_resource_binding_evidence (
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('user', 'deployment')),
    owner_id TEXT NOT NULL CHECK (length(owner_id) > 0),
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    installed_revision INTEGER NOT NULL CHECK (installed_revision BETWEEN 1 AND 9007199254740991),
    resource_kind TEXT NOT NULL CHECK (length(resource_kind) > 0),
    local_name TEXT NOT NULL CHECK (length(local_name) > 0),
    binding_id TEXT NOT NULL CHECK (length(binding_id) > 0),
    provider_identity TEXT NOT NULL CHECK (length(provider_identity) > 0),
    actual_json TEXT CHECK (actual_json IS NULL OR json_valid(actual_json)),
    state TEXT NOT NULL CHECK (state IN ('available', 'unavailable', 'stale')),
    materialized_at INTEGER NOT NULL CHECK (materialized_at BETWEEN 0 AND 9007199254740991),
    error TEXT,
    PRIMARY KEY (owner_kind, owner_id, participant_id, installed_revision, resource_kind, local_name),
    UNIQUE (owner_kind, owner_id, participant_id, installed_revision, binding_id),
    FOREIGN KEY (participant_id, installed_revision) REFERENCES auth_installed_participants(participant_id, revision)
);

CREATE TABLE auth_user_profiles (
    principal_id TEXT PRIMARY KEY REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    display_name TEXT CHECK (display_name IS NULL OR length(display_name) > 0),
    email TEXT,
    image_url TEXT,
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991)
);

CREATE TABLE auth_local_credentials (
    principal_id TEXT PRIMARY KEY REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    normalized_username TEXT NOT NULL UNIQUE CHECK (length(normalized_username) > 0),
    password_hash TEXT NOT NULL CHECK (length(password_hash) > 0),
    hash_profile INTEGER NOT NULL CHECK (hash_profile >= 1),
    failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    locked_until INTEGER CHECK (locked_until BETWEEN 0 AND 9007199254740991),
    password_changed_at INTEGER NOT NULL CHECK (password_changed_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991)
);






CREATE TABLE auth_account_flows (
    flow_id TEXT PRIMARY KEY CHECK (length(flow_id) > 0),
    kind TEXT NOT NULL CHECK (kind IN ('admin_account', 'identity_link', 'password_reset')),
    token_hash TEXT NOT NULL UNIQUE CHECK (length(token_hash) = 43),
    target_principal_id TEXT REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    target_provider_id TEXT,
    return_location TEXT,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    state TEXT NOT NULL CHECK (state IN ('pending', 'consumed', 'expired', 'revoked')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    expires_at INTEGER NOT NULL CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    consumed_at INTEGER CHECK (consumed_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    CHECK (expires_at >= created_at),
    CHECK ((state = 'consumed') = (consumed_at IS NOT NULL))
);
CREATE INDEX auth_account_flows_target_idx
    ON auth_account_flows(target_principal_id, kind, state);

CREATE TABLE auth_provisioned_identities (
    identity_key_id TEXT PRIMARY KEY CHECK (length(identity_key_id) = 43),
    identity_public_key TEXT NOT NULL UNIQUE CHECK (length(identity_public_key) = 43),
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    deployment_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    kind TEXT NOT NULL CHECK (kind IN ('service', 'device')),
    state TEXT NOT NULL CHECK (state IN ('active', 'revoked')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    revoked_at INTEGER CHECK (revoked_at BETWEEN 0 AND 9007199254740991),
    UNIQUE (identity_key_id, principal_id),
    FOREIGN KEY (deployment_id, participant_id)
        REFERENCES auth_deployments(deployment_id, participant_id),
    FOREIGN KEY (instance_id, deployment_id)
        REFERENCES auth_instances(instance_id, deployment_id) ON DELETE CASCADE,
    CHECK ((state = 'revoked') = (revoked_at IS NOT NULL))
);

CREATE TABLE auth_device_provisioning_secrets (
    secret_id TEXT PRIMARY KEY CHECK (length(secret_id) > 0),
    instance_id TEXT NOT NULL REFERENCES auth_instances(instance_id) ON DELETE CASCADE,
    secret_hash TEXT NOT NULL UNIQUE CHECK (length(secret_hash) = 43),
    state TEXT NOT NULL CHECK (state IN ('pending', 'consumed', 'expired', 'revoked')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    expires_at INTEGER NOT NULL CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    consumed_at INTEGER CHECK (consumed_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    CHECK (expires_at >= created_at),
    CHECK ((state = 'consumed') = (consumed_at IS NOT NULL))
);

CREATE TABLE auth_device_activation_reviews (
    review_id TEXT PRIMARY KEY CHECK (length(review_id) > 0),
    principal_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    request_digest TEXT NOT NULL CHECK (length(request_digest) = 43),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    state TEXT NOT NULL CHECK (state IN ('pending', 'approved', 'rejected', 'expired')),
    requested_at INTEGER NOT NULL CHECK (requested_at BETWEEN 0 AND 9007199254740991),
    expires_at INTEGER NOT NULL CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    activated_by_user_principal_id TEXT,
    decided_at INTEGER CHECK (decided_at BETWEEN 0 AND 9007199254740991),
    decided_by TEXT,
    reason TEXT,
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    FOREIGN KEY (principal_id, deployment_id)
        REFERENCES auth_devices(principal_id, deployment_id) ON DELETE CASCADE,
    FOREIGN KEY (instance_id, deployment_id)
        REFERENCES auth_instances(instance_id, deployment_id) ON DELETE CASCADE,
    CHECK (expires_at >= requested_at),
    CHECK ((decided_at IS NULL) = (decided_by IS NULL)),
    CHECK (state NOT IN ('approved', 'rejected') OR decided_at IS NOT NULL),
    CHECK (state != 'pending' OR decided_at IS NULL)
);
CREATE INDEX auth_device_activation_reviews_state_expiry
    ON auth_device_activation_reviews(state, expires_at);

CREATE TABLE auth_idempotency_results (
    scope_key TEXT PRIMARY KEY CHECK (length(scope_key) = 43),
    purpose TEXT NOT NULL CHECK (length(purpose) > 0),
    signer_id TEXT NOT NULL CHECK (length(signer_id) > 0),
    request_id TEXT NOT NULL CHECK (length(request_id) > 0),
    request_digest TEXT NOT NULL CHECK (length(request_digest) = 43),
    result_json TEXT NOT NULL CHECK (json_valid(result_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    expires_at INTEGER NOT NULL CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    CHECK (expires_at >= created_at),
    UNIQUE (purpose, signer_id, request_id)
);

CREATE TABLE auth_authorization_issuers (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) = 43),
    public_key TEXT NOT NULL CHECK (length(public_key) = 43),
    is_current INTEGER NOT NULL CHECK (is_current IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('active', 'retired', 'compromised')),
    activated_at INTEGER NOT NULL,
    retired_at INTEGER,
    maximum_acceptance_deadline INTEGER NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    revoked_at INTEGER CHECK (revoked_at BETWEEN 0 AND 9007199254740991),
    CHECK (is_current = 0 OR (revoked_at IS NULL AND state = 'active')),
    CHECK ((state = 'compromised') = (revoked_at IS NOT NULL)),
    CHECK (state != 'retired' OR retired_at IS NOT NULL)
);
CREATE UNIQUE INDEX idx_auth_authorization_current_issuer
    ON auth_authorization_issuers(is_current) WHERE is_current = 1;
CREATE TRIGGER auth_authorization_issuer_revocation_is_final
BEFORE UPDATE OF revoked_at ON auth_authorization_issuers
WHEN OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS NOT OLD.revoked_at
BEGIN
    SELECT RAISE(ABORT, 'issuer revocation is irreversible');
END;
CREATE TRIGGER auth_authorization_issuer_retirement_final BEFORE UPDATE ON auth_authorization_issuers
WHEN (OLD.state != 'active' AND NEW.state = 'active')
    OR (OLD.state = 'compromised' AND NEW.state != 'compromised')
    OR NEW.key_id IS NOT OLD.key_id OR NEW.public_key IS NOT OLD.public_key
    OR NEW.maximum_acceptance_deadline < OLD.maximum_acceptance_deadline
BEGIN SELECT RAISE(ABORT, 'issuer identity and retirement cannot roll back'); END;












CREATE TABLE auth_post_commit_actions (
    action_id TEXT PRIMARY KEY CHECK (length(action_id) = 43),
    kind TEXT NOT NULL CHECK (kind IN ('event', 'kick', 'authority_publish', 'session_revoke', 'resource_reconcile', 'transport_reevaluate')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at BETWEEN 0 AND 9007199254740991),
    claimed_until INTEGER CHECK (claimed_until BETWEEN 0 AND 9007199254740991),
    claim_token TEXT CHECK (claim_token IS NULL OR length(claim_token) = 26),
    last_error TEXT,
    predecessor_action_id TEXT,
    enforcement_work_id TEXT REFERENCES auth_enforcement_work(work_id),
    cursor TEXT,
    event_delivery_json TEXT CHECK (event_delivery_json IS NULL OR json_valid(event_delivery_json))
);
CREATE INDEX auth_post_commit_actions_ready_idx
    ON auth_post_commit_actions(next_attempt_at, action_id);
CREATE INDEX auth_post_commit_actions_predecessor
    ON auth_post_commit_actions(predecessor_action_id);



CREATE TABLE auth_bootstrap_administrator (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    principal_id TEXT NOT NULL UNIQUE REFERENCES auth_principals(principal_id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991)
);

CREATE TABLE auth_roles (
    role_id TEXT NOT NULL CHECK (length(role_id) = 26),
    identity_generation INTEGER NOT NULL CHECK (identity_generation >= 1),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 256),
    description TEXT NOT NULL CHECK (length(description) <= 4096),
    state TEXT NOT NULL CHECK (state IN ('active', 'deleted')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (role_id, identity_generation)
);
CREATE UNIQUE INDEX auth_roles_active_identity ON auth_roles(role_id) WHERE state = 'active';

CREATE TABLE auth_accepted_apis (
    api_id TEXT PRIMARY KEY CHECK (length(api_id) > 0),
    generation INTEGER NOT NULL CHECK (generation >= 1),
    accepted_revision INTEGER NOT NULL CHECK (accepted_revision >= 1),
    definition_digest TEXT NOT NULL CHECK (length(definition_digest) = 43),
    definition_json TEXT NOT NULL CHECK (json_valid(definition_json)),
    accepted_at INTEGER NOT NULL,
    accepted_by TEXT NOT NULL REFERENCES auth_principals(principal_id),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    UNIQUE (api_id, generation)
);
CREATE TABLE auth_api_verification_snapshots (
    snapshot_digest TEXT PRIMARY KEY CHECK (length(snapshot_digest) = 43),
    api_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 1),
    accepted_revision INTEGER NOT NULL CHECK (accepted_revision >= 1),
    issuer_key_id TEXT NOT NULL REFERENCES auth_authorization_issuers(key_id),
    signed_bytes BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (api_id, generation, accepted_revision)
);
CREATE TRIGGER auth_api_snapshot_immutable BEFORE UPDATE ON auth_api_verification_snapshots
BEGIN SELECT RAISE(ABORT, 'API verification snapshots are immutable'); END;
CREATE TRIGGER auth_api_snapshot_retained BEFORE DELETE ON auth_api_verification_snapshots
BEGIN SELECT RAISE(ABORT, 'API verification snapshots are retained'); END;

CREATE TABLE auth_capability_identities (
    capability_id TEXT NOT NULL,
    identity_generation INTEGER NOT NULL CHECK (identity_generation >= 1),
    api_id TEXT NOT NULL REFERENCES auth_accepted_apis(api_id),
    consent_revision INTEGER NOT NULL CHECK (consent_revision >= 1),
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    consequence TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'deleted')),
    created_at INTEGER NOT NULL,
    deleted_at INTEGER,
    PRIMARY KEY (capability_id, identity_generation),
    CHECK ((state = 'deleted') = (deleted_at IS NOT NULL))
);
CREATE UNIQUE INDEX auth_capability_active_identity ON auth_capability_identities(capability_id)
WHERE state = 'active';
CREATE INDEX auth_capability_api ON auth_capability_identities(api_id, state);
CREATE TRIGGER auth_capability_consent_monotonic BEFORE UPDATE ON auth_capability_identities
WHEN NEW.consent_revision < OLD.consent_revision
    OR NEW.capability_id IS NOT OLD.capability_id
    OR NEW.identity_generation IS NOT OLD.identity_generation
    OR NEW.api_id IS NOT OLD.api_id
    OR (OLD.state = 'deleted' AND NEW.state != 'deleted')
BEGIN SELECT RAISE(ABORT, 'capability identity and consent cannot roll back'); END;

CREATE TABLE auth_api_actions (
    api_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    action_key TEXT NOT NULL,
    introduced_revision INTEGER NOT NULL CHECK (introduced_revision >= 1),
    descriptor_json TEXT NOT NULL CHECK (json_valid(descriptor_json)),
    PRIMARY KEY (api_id, generation, action_key),
    FOREIGN KEY (api_id, generation) REFERENCES auth_accepted_apis(api_id, generation)
        ON DELETE CASCADE
);
CREATE TABLE auth_capability_actions (
    capability_id TEXT NOT NULL,
    identity_generation INTEGER NOT NULL,
    api_id TEXT NOT NULL,
    api_generation INTEGER NOT NULL,
    action_key TEXT NOT NULL,
    member_since_revision INTEGER NOT NULL CHECK (member_since_revision >= 1),
    PRIMARY KEY (capability_id, identity_generation, api_id, api_generation, action_key),
    FOREIGN KEY (capability_id, identity_generation)
        REFERENCES auth_capability_identities(capability_id, identity_generation),
    FOREIGN KEY (api_id, api_generation, action_key)
        REFERENCES auth_api_actions(api_id, generation, action_key) ON DELETE CASCADE
);
CREATE TABLE auth_role_capabilities (
    role_id TEXT NOT NULL,
    role_generation INTEGER NOT NULL,
    capability_id TEXT NOT NULL,
    capability_generation INTEGER NOT NULL,
    PRIMARY KEY (role_id, role_generation, capability_id, capability_generation),
    FOREIGN KEY (role_id, role_generation) REFERENCES auth_roles(role_id, identity_generation),
    FOREIGN KEY (capability_id, capability_generation)
        REFERENCES auth_capability_identities(capability_id, identity_generation)
);
CREATE TABLE auth_role_assignments (
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    role_id TEXT NOT NULL,
    role_generation INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (principal_id, role_id, role_generation),
    FOREIGN KEY (role_id, role_generation) REFERENCES auth_roles(role_id, identity_generation)
);
CREATE INDEX auth_role_assignments_role ON auth_role_assignments(role_id, role_generation, principal_id);
CREATE TABLE auth_direct_capabilities (
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    capability_id TEXT NOT NULL,
    capability_generation INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (principal_id, capability_id, capability_generation),
    FOREIGN KEY (capability_id, capability_generation)
        REFERENCES auth_capability_identities(capability_id, identity_generation)
);
CREATE INDEX auth_direct_capabilities_identity
ON auth_direct_capabilities(capability_id, capability_generation, principal_id);

CREATE TABLE auth_oidc_providers (
    provider_id TEXT PRIMARY KEY CHECK (length(provider_id) = 26),
    issuer TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    client_id TEXT NOT NULL,
    sealed_client_secret BLOB,
    config_json TEXT NOT NULL CHECK (json_valid(config_json)),
    claim_freshness_seconds INTEGER NOT NULL CHECK (claim_freshness_seconds >= 1),
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled', 'deleted')),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE auth_oidc_role_mappings (
    mapping_id TEXT PRIMARY KEY CHECK (length(mapping_id) = 26),
    provider_id TEXT NOT NULL REFERENCES auth_oidc_providers(provider_id),
    claim_name TEXT NOT NULL,
    claim_value TEXT NOT NULL,
    role_id TEXT NOT NULL,
    role_generation INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    FOREIGN KEY (role_id, role_generation) REFERENCES auth_roles(role_id, identity_generation),
    UNIQUE (provider_id, claim_name, claim_value, role_id, role_generation)
);
CREATE TABLE auth_login_sessions (
    login_session_id TEXT PRIMARY KEY CHECK (length(login_session_id) = 26),
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    method TEXT NOT NULL CHECK (method IN ('local', 'oidc')),
    provider_id TEXT REFERENCES auth_oidc_providers(provider_id),
    upstream_issuer TEXT,
    upstream_subject TEXT,
    verified_claims_json TEXT CHECK (verified_claims_json IS NULL OR json_valid(verified_claims_json)),
    verified_roles_json TEXT NOT NULL CHECK (json_valid(verified_roles_json)),
    observed_at INTEGER NOT NULL,
    observation_order INTEGER NOT NULL CHECK (observation_order >= 1),
    fresh_until INTEGER,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    CHECK (expires_at > created_at),
    UNIQUE (login_session_id, principal_id),
    CHECK ((method = 'oidc') = (provider_id IS NOT NULL)),
    CHECK (method != 'oidc' OR (upstream_issuer IS NOT NULL AND upstream_subject IS NOT NULL
        AND fresh_until IS NOT NULL AND verified_claims_json IS NOT NULL))
);
CREATE INDEX auth_login_sessions_principal ON auth_login_sessions(principal_id, revoked_at, expires_at);
CREATE INDEX auth_login_sessions_provider ON auth_login_sessions(provider_id, principal_id, observation_order);

CREATE TABLE auth_oauth_clients (
    client_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('browser', 'native')),
    display_name TEXT NOT NULL,
    redirect_uris_json TEXT NOT NULL CHECK (json_valid(redirect_uris_json)),
    development INTEGER NOT NULL CHECK (development IN (0, 1)),
    implied_capabilities_json TEXT NOT NULL CHECK (json_valid(implied_capabilities_json)),
    requested_privileges_json TEXT NOT NULL CHECK (json_valid(requested_privileges_json)),
    metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled', 'deleted')),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE auth_platform_privileges (
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    privilege TEXT NOT NULL CHECK (privilege IN ('principals.manage', 'roles.manage', 'apis.accept',
        'apis.forceReplace', 'clients.manage', 'privileges.manage')),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (principal_id, privilege)
);
CREATE TABLE auth_platform_delegations (
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    client_id TEXT NOT NULL REFERENCES auth_oauth_clients(client_id),
    binding_kind TEXT NOT NULL CHECK (binding_kind IN ('browser', 'native')),
    binding_value TEXT NOT NULL,
    approved_privileges_json TEXT NOT NULL CHECK (json_valid(approved_privileges_json)),
    expires_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    PRIMARY KEY (principal_id, client_id, binding_kind, binding_value)
);
CREATE TABLE auth_remembered_consent (
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    client_id TEXT NOT NULL,
    binding_kind TEXT NOT NULL CHECK (binding_kind IN ('browser', 'native')),
    binding_value TEXT NOT NULL,
    capability_id TEXT NOT NULL,
    capability_generation INTEGER NOT NULL,
    consent_revision INTEGER NOT NULL CHECK (consent_revision >= 1),
    decision TEXT NOT NULL CHECK (decision IN ('approved', 'declined')),
    expires_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (principal_id, client_id, binding_kind, binding_value,
        capability_id, capability_generation, consent_revision),
    FOREIGN KEY (capability_id, capability_generation)
        REFERENCES auth_capability_identities(capability_id, identity_generation)
);
CREATE INDEX auth_consent_binding
ON auth_remembered_consent(principal_id, client_id, binding_kind, binding_value, expires_at);

CREATE TABLE auth_oauth_grants (
    oauth_grant_id TEXT PRIMARY KEY CHECK (length(oauth_grant_id) = 26),
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    login_session_id TEXT NOT NULL REFERENCES auth_login_sessions(login_session_id),
    client_id TEXT NOT NULL,
    binding_kind TEXT NOT NULL CHECK (binding_kind IN ('browser', 'native')),
    binding_value TEXT NOT NULL,
    durable_dpop_jkt TEXT NOT NULL CHECK (length(durable_dpop_jkt) = 43),
    public_jwk_json TEXT NOT NULL CHECK (json_valid(public_jwk_json)
        AND json_extract(public_jwk_json, '$.d') IS NULL),
    required_capabilities_json TEXT NOT NULL CHECK (json_valid(required_capabilities_json)),
    optional_capabilities_json TEXT NOT NULL CHECK (json_valid(optional_capabilities_json)),
    approved_capabilities_json TEXT NOT NULL CHECK (json_valid(approved_capabilities_json)),
    approved_privileges_json TEXT NOT NULL CHECK (json_valid(approved_privileges_json)),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    CHECK (expires_at > created_at),
    UNIQUE (oauth_grant_id, principal_id),
    FOREIGN KEY (login_session_id, principal_id)
        REFERENCES auth_login_sessions(login_session_id, principal_id)
);
CREATE INDEX auth_oauth_grants_login ON auth_oauth_grants(login_session_id, revoked_at, expires_at);
CREATE INDEX auth_oauth_grants_binding
ON auth_oauth_grants(principal_id, client_id, binding_kind, binding_value, revoked_at);
CREATE TABLE auth_oauth_families (
    family_lookup_hash TEXT PRIMARY KEY CHECK (length(family_lookup_hash) = 43),
    oauth_grant_id TEXT NOT NULL REFERENCES auth_oauth_grants(oauth_grant_id),
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision >= 1)
);
CREATE INDEX auth_oauth_families_grant ON auth_oauth_families(oauth_grant_id, revoked_at);
CREATE TRIGGER auth_oauth_family_revocation_final BEFORE UPDATE ON auth_oauth_families
WHEN OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS NOT OLD.revoked_at
BEGIN SELECT RAISE(ABORT, 'refresh family revocation is irreversible'); END;
CREATE TABLE auth_oauth_artifacts (
    kind TEXT NOT NULL CHECK (kind IN ('authorization', 'code', 'device', 'token', 'refresh', 'dpopReplay')),
    lookup_hash TEXT NOT NULL CHECK (length(lookup_hash) = 43),
    family_lookup_hash TEXT REFERENCES auth_oauth_families(family_lookup_hash),
    oauth_grant_id TEXT REFERENCES auth_oauth_grants(oauth_grant_id),
    sealed_record BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    consumed_at INTEGER,
    PRIMARY KEY (kind, lookup_hash)
);
CREATE INDEX auth_oauth_artifacts_expiry ON auth_oauth_artifacts(expires_at);
CREATE INDEX auth_oauth_artifacts_family ON auth_oauth_artifacts(family_lookup_hash, kind);

CREATE TABLE auth_authorization_sessions (
    authorization_session_id TEXT PRIMARY KEY CHECK (length(authorization_session_id) = 26),
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    binding_kind TEXT NOT NULL CHECK (binding_kind IN ('browser', 'native', 'service', 'device')),
    binding_key TEXT NOT NULL CHECK (length(binding_key) = 43),
    binding_json TEXT NOT NULL CHECK (json_valid(binding_json)),
    runtime_id TEXT NOT NULL,
    session_public_key TEXT NOT NULL CHECK (length(session_public_key) = 43),
    credential_kind TEXT NOT NULL CHECK (credential_kind IN ('oauthGrant', 'provisionedIdentity')),
    oauth_grant_id TEXT REFERENCES auth_oauth_grants(oauth_grant_id),
    identity_key_id TEXT REFERENCES auth_provisioned_identities(identity_key_id),
    selected_capabilities_json TEXT NOT NULL CHECK (json_valid(selected_capabilities_json)),
    approved_capabilities_json TEXT NOT NULL CHECK (json_valid(approved_capabilities_json)),
    approved_privileges_json TEXT NOT NULL CHECK (json_valid(approved_privileges_json)),
    issued_at INTEGER NOT NULL,
    hard_deadline INTEGER NOT NULL,
    latest_context_expiry INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'superseded', 'revoked')),
    retired_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    CHECK ((credential_kind = 'oauthGrant' AND oauth_grant_id IS NOT NULL AND identity_key_id IS NULL
            AND binding_kind IN ('browser', 'native'))
        OR (credential_kind = 'provisionedIdentity' AND identity_key_id IS NOT NULL
            AND oauth_grant_id IS NULL AND binding_kind IN ('service', 'device'))),
    CHECK (issued_at < hard_deadline AND latest_context_expiry <= hard_deadline),
    CHECK ((state = 'active') = (retired_at IS NULL)),
    FOREIGN KEY (oauth_grant_id, principal_id)
        REFERENCES auth_oauth_grants(oauth_grant_id, principal_id),
    FOREIGN KEY (identity_key_id, principal_id)
        REFERENCES auth_provisioned_identities(identity_key_id, principal_id)
);
CREATE UNIQUE INDEX auth_authorization_session_active_runtime
ON auth_authorization_sessions(binding_key, runtime_id, session_public_key) WHERE state = 'active';
CREATE INDEX auth_authorization_sessions_principal
ON auth_authorization_sessions(principal_id, state, authorization_session_id);
CREATE INDEX auth_authorization_sessions_grant
ON auth_authorization_sessions(oauth_grant_id, state, authorization_session_id);
CREATE INDEX auth_authorization_sessions_identity
ON auth_authorization_sessions(identity_key_id, state, authorization_session_id);
CREATE INDEX auth_authorization_sessions_runtime
ON auth_authorization_sessions(runtime_id, state, authorization_session_id);
CREATE INDEX auth_authorization_sessions_expiry ON auth_authorization_sessions(hard_deadline, state);
CREATE TRIGGER auth_authorization_session_retirement_final BEFORE UPDATE ON auth_authorization_sessions
WHEN OLD.state != 'active' AND (NEW.state IS NOT OLD.state OR NEW.retired_at IS NOT OLD.retired_at)
BEGIN SELECT RAISE(ABORT, 'logical authorization-session retirement is irreversible'); END;
CREATE TRIGGER auth_authorization_session_identity_immutable BEFORE UPDATE ON auth_authorization_sessions
WHEN NEW.authorization_session_id IS NOT OLD.authorization_session_id
    OR NEW.principal_id IS NOT OLD.principal_id OR NEW.binding_kind IS NOT OLD.binding_kind
    OR NEW.binding_key IS NOT OLD.binding_key OR NEW.binding_json IS NOT OLD.binding_json
    OR NEW.runtime_id IS NOT OLD.runtime_id OR NEW.session_public_key IS NOT OLD.session_public_key
    OR NEW.credential_kind IS NOT OLD.credential_kind OR NEW.oauth_grant_id IS NOT OLD.oauth_grant_id
    OR NEW.identity_key_id IS NOT OLD.identity_key_id OR NEW.issued_at IS NOT OLD.issued_at
    OR NEW.hard_deadline > OLD.hard_deadline OR NEW.latest_context_expiry < OLD.latest_context_expiry
BEGIN SELECT RAISE(ABORT, 'logical authorization identity and acceptance history cannot change'); END;

CREATE TABLE auth_session_authorities (
    context_digest TEXT PRIMARY KEY CHECK (length(context_digest) = 43),
    authorization_session_id TEXT NOT NULL REFERENCES auth_authorization_sessions(authorization_session_id),
    issuer_key_id TEXT NOT NULL REFERENCES auth_authorization_issuers(key_id),
    signed_bytes BLOB NOT NULL,
    api_snapshots_json TEXT NOT NULL CHECK (json_valid(api_snapshots_json)),
    issued_at INTEGER NOT NULL,
    not_before INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    maximum_acceptance_deadline INTEGER NOT NULL,
    CHECK (not_before <= issued_at AND issued_at < expires_at
        AND expires_at <= maximum_acceptance_deadline)
);
CREATE INDEX auth_session_authorities_session_expiry
ON auth_session_authorities(authorization_session_id, expires_at);
CREATE INDEX auth_session_authorities_issuer ON auth_session_authorities(issuer_key_id);
CREATE TRIGGER auth_session_authority_immutable BEFORE UPDATE ON auth_session_authorities
BEGIN SELECT RAISE(ABORT, 'signed session authority is immutable'); END;
CREATE TRIGGER auth_session_authority_retained BEFORE DELETE ON auth_session_authorities
BEGIN SELECT RAISE(ABORT, 'signed session authority is retained'); END;

CREATE TABLE auth_authorization_revocations (
    authorization_session_id TEXT PRIMARY KEY REFERENCES auth_authorization_sessions(authorization_session_id),
    effective_cutoff INTEGER NOT NULL,
    reason TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('reduction', 'hard')),
    issuer_key_id TEXT NOT NULL REFERENCES auth_authorization_issuers(key_id),
    signed_bytes BLOB NOT NULL,
    latest_context_expiry INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TRIGGER auth_authorization_revocation_immutable BEFORE UPDATE ON auth_authorization_revocations
BEGIN SELECT RAISE(ABORT, 'logical-session revocation is immutable'); END;
CREATE TRIGGER auth_authorization_revocation_retained BEFORE DELETE ON auth_authorization_revocations
BEGIN SELECT RAISE(ABORT, 'historical revocation cutoffs are retained'); END;

CREATE TABLE auth_attachments (
    attachment_id TEXT PRIMARY KEY CHECK (length(attachment_id) = 26),
    server_id TEXT NOT NULL,
    broker_client_id INTEGER NOT NULL CHECK (broker_client_id >= 1),
    authorization_session_id TEXT NOT NULL REFERENCES auth_authorization_sessions(authorization_session_id),
    ephemeral_nkey TEXT NOT NULL CHECK (length(ephemeral_nkey) = 56),
    transport_policy_digest TEXT NOT NULL CHECK (length(transport_policy_digest) = 43),
    state TEXT NOT NULL CHECK (state IN ('pending', 'admitted', 'closed')),
    created_at INTEGER NOT NULL,
    admitted_at INTEGER,
    closed_at INTEGER,
    UNIQUE (server_id, broker_client_id, attachment_id)
);
CREATE INDEX auth_attachments_session ON auth_attachments(authorization_session_id, state, attachment_id);
CREATE INDEX auth_attachments_broker ON auth_attachments(server_id, broker_client_id, state);

CREATE TABLE auth_enforcement_work (
    work_id TEXT PRIMARY KEY CHECK (length(work_id) = 26),
    scope_kind TEXT NOT NULL,
    scope_key TEXT NOT NULL,
    scope_json TEXT NOT NULL CHECK (json_valid(scope_json)),
    cursor TEXT,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'completed', 'failed')),
    scanned_sessions INTEGER NOT NULL DEFAULT 0 CHECK (scanned_sessions >= 0),
    retired_sessions INTEGER NOT NULL DEFAULT 0 CHECK (retired_sessions >= 0),
    pending_kicks INTEGER NOT NULL DEFAULT 0 CHECK (pending_kicks >= 0),
    failed_kicks INTEGER NOT NULL DEFAULT 0 CHECK (failed_kicks >= 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL,
    claimed_until INTEGER,
    claim_token TEXT,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER
);
CREATE INDEX auth_enforcement_work_ready ON auth_enforcement_work(state, next_attempt_at, work_id);

CREATE TABLE auth_security_audit (
    audit_id TEXT PRIMARY KEY CHECK (length(audit_id) = 26),
    actor_principal_id TEXT REFERENCES auth_principals(principal_id),
    kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    data_json TEXT NOT NULL CHECK (json_valid(data_json)),
    created_at INTEGER NOT NULL
);
CREATE INDEX auth_security_audit_target ON auth_security_audit(target_id, created_at, audit_id);
CREATE TRIGGER auth_security_audit_immutable BEFORE UPDATE ON auth_security_audit
BEGIN SELECT RAISE(ABORT, 'security audit entries are immutable'); END;
CREATE TRIGGER auth_security_audit_retained BEFORE DELETE ON auth_security_audit
BEGIN SELECT RAISE(ABORT, 'security audit entries are retained'); END;

CREATE INDEX auth_role_capabilities_identity
ON auth_role_capabilities(capability_id, capability_generation, role_id, role_generation);
CREATE INDEX auth_oauth_grants_client ON auth_oauth_grants(client_id, revoked_at, oauth_grant_id);
CREATE INDEX auth_platform_delegations_client ON auth_platform_delegations(client_id, principal_id);
CREATE INDEX auth_provisioned_identities_principal
ON auth_provisioned_identities(principal_id, state, identity_key_id);
CREATE INDEX auth_provisioned_identities_deployment
ON auth_provisioned_identities(deployment_id, instance_id, state);

CREATE TABLE auth_deployment_bindings (
    deployment_id TEXT PRIMARY KEY REFERENCES auth_deployments(deployment_id),
    installed_revision INTEGER NOT NULL CHECK (installed_revision >= 1),
    implementation_digest TEXT NOT NULL CHECK (length(implementation_digest) = 43),
    provided_apis_json TEXT NOT NULL CHECK (json_valid(provided_apis_json)),
    required_capabilities_json TEXT NOT NULL CHECK (json_valid(required_capabilities_json)),
    optional_capabilities_json TEXT NOT NULL CHECK (json_valid(optional_capabilities_json)),
    resource_commitments_json TEXT NOT NULL CHECK (json_valid(resource_commitments_json)),
    revision INTEGER NOT NULL CHECK (revision >= 1)
);
CREATE TABLE auth_provider_certificates (
    certificate_digest TEXT PRIMARY KEY CHECK (length(certificate_digest) = 43),
    deployment_id TEXT NOT NULL REFERENCES auth_deployments(deployment_id),
    instance_id TEXT NOT NULL REFERENCES auth_instances(instance_id),
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    api_id TEXT NOT NULL,
    api_generation INTEGER NOT NULL CHECK (api_generation >= 1),
    implementation_digest TEXT NOT NULL CHECK (length(implementation_digest) = 43),
    issuer_key_id TEXT NOT NULL REFERENCES auth_authorization_issuers(key_id),
    implemented_actions_json TEXT NOT NULL CHECK (json_valid(implemented_actions_json)),
    signed_bytes BLOB NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    CHECK (expires_at > issued_at)
);
CREATE INDEX auth_provider_certificates_deployment
ON auth_provider_certificates(deployment_id, api_id, api_generation);
CREATE TRIGGER auth_provider_certificate_immutable BEFORE UPDATE ON auth_provider_certificates
BEGIN SELECT RAISE(ABORT, 'provider certificates are immutable'); END;
CREATE TRIGGER auth_provider_certificate_retained BEFORE DELETE ON auth_provider_certificates
BEGIN SELECT RAISE(ABORT, 'provider certificates are retained'); END;

CREATE TABLE auth_device_companions (
    instance_id TEXT PRIMARY KEY REFERENCES auth_instances(instance_id),
    required INTEGER NOT NULL CHECK (required IN (0, 1)),
    companion_request_id TEXT,
    oauth_grant_id TEXT REFERENCES auth_oauth_grants(oauth_grant_id),
    approved_capabilities_json TEXT NOT NULL CHECK (json_valid(approved_capabilities_json)),
    state TEXT NOT NULL CHECK (state IN ('active', 'missing', 'revoked')),
    expires_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    CHECK (state != 'active' OR oauth_grant_id IS NOT NULL)
);

CREATE TABLE auth_issuer_rotations (
    sequence INTEGER PRIMARY KEY CHECK (sequence >= 1),
    previous_key_id TEXT NOT NULL REFERENCES auth_authorization_issuers(key_id),
    next_key_id TEXT NOT NULL UNIQUE REFERENCES auth_authorization_issuers(key_id),
    trellis_instance_id TEXT NOT NULL CHECK (length(trellis_instance_id) = 26),
    audience_nats_account TEXT NOT NULL,
    signed_bytes BLOB NOT NULL,
    activated_at INTEGER NOT NULL
);
CREATE TRIGGER auth_issuer_rotation_immutable BEFORE UPDATE ON auth_issuer_rotations
BEGIN SELECT RAISE(ABORT, 'issuer rotations are immutable'); END;
CREATE TRIGGER auth_issuer_rotation_retained BEFORE DELETE ON auth_issuer_rotations
BEGIN SELECT RAISE(ABORT, 'issuer rotations are retained'); END;
CREATE INDEX auth_device_companions_grant ON auth_device_companions(oauth_grant_id, instance_id);
