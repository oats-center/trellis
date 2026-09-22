PRAGMA foreign_keys = ON;

CREATE TABLE trellis_platform_store_marker (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO trellis_platform_store_marker (id) VALUES (1);

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
    participant_kind TEXT NOT NULL CHECK (participant_kind IN ('service', 'app', 'device', 'agent')),
    participant_digest TEXT NOT NULL CHECK (length(participant_digest) = 43),
    needs_digest TEXT NOT NULL CHECK (length(needs_digest) = 43),
    package_digest TEXT NOT NULL REFERENCES auth_package_evidence(package_digest),
    evidence_digest TEXT NOT NULL REFERENCES auth_package_evidence_documents(evidence_digest),
    participant_path TEXT NOT NULL CHECK (length(participant_path) > 0),
    companion_participant_id TEXT CHECK (companion_participant_id IS NULL OR length(companion_participant_id) > 0),
    companion_participant_kind TEXT CHECK (companion_participant_kind IN ('service', 'app', 'device', 'agent')),
    companion_required INTEGER NOT NULL CHECK (companion_required IN (0, 1)),
    projection_json TEXT NOT NULL CHECK (json_valid(projection_json)),
    installed_at INTEGER NOT NULL CHECK (installed_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY (participant_id, revision),
    CHECK ((companion_participant_id IS NULL) = (companion_participant_kind IS NULL)),
    CHECK (companion_required = 0 OR companion_participant_id IS NOT NULL)
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

CREATE TABLE auth_api_bindings (
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    api_id TEXT NOT NULL CHECK (length(api_id) > 0),
    provider_deployment_id TEXT NOT NULL CHECK (length(provider_deployment_id) > 0),
    PRIMARY KEY (participant_id, api_id)
);

CREATE TABLE auth_grant_bindings (
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('deployment', 'user')),
    owner_id TEXT NOT NULL CHECK (length(owner_id) > 0),
    participant_id TEXT NOT NULL,
    installed_revision INTEGER NOT NULL CHECK (installed_revision BETWEEN 1 AND 9007199254740991),
    grants_json TEXT NOT NULL CHECK (json_valid(grants_json)),
    approval_mode TEXT NOT NULL CHECK (approval_mode IN ('exact', 'capabilities')),
    approved_capabilities_json TEXT NOT NULL CHECK (json_valid(approved_capabilities_json)),
    approved_resources_json TEXT NOT NULL CHECK (json_valid(approved_resources_json)),
    delegation_ceiling_json TEXT NOT NULL CHECK (json_valid(delegation_ceiling_json)),
    approval_decision_digest TEXT NOT NULL CHECK (length(approval_decision_digest) = 43),
    approval_expected_grant_revision INTEGER NOT NULL CHECK (approval_expected_grant_revision BETWEEN 0 AND 9007199254740991),
    companion_approved INTEGER NOT NULL CHECK (companion_approved IN (0, 1)),
    platform_privileges_json TEXT NOT NULL CHECK (json_valid(platform_privileges_json)),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
    state TEXT NOT NULL CHECK (state IN ('active', 'revoked')),
    expires_at INTEGER CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    provenance_json TEXT CHECK (provenance_json IS NULL OR json_valid(provenance_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY (owner_kind, owner_id, participant_id),
    FOREIGN KEY (participant_id, installed_revision)
        REFERENCES auth_installed_participants(participant_id, revision),
    CHECK (state != 'revoked' OR (
        json_array_length(grants_json, '$.permissions') = 0
        AND json_array_length(approved_capabilities_json) = 0
        AND json_array_length(approved_resources_json) = 0
        AND json_array_length(platform_privileges_json) = 0
    )),
    CHECK (approval_mode != 'exact'
        OR json_type(delegation_ceiling_json, '$.exactRestrictions') IS NOT NULL),
    CHECK (approval_mode != 'exact'
        OR json_array_length(delegation_ceiling_json, '$.capabilities') = 0),
    CHECK (approval_mode != 'capabilities'
        OR json_type(delegation_ceiling_json, '$.exactRestrictions') IS NULL
        OR json_type(delegation_ceiling_json, '$.exactRestrictions') = 'null'),
    CHECK (approval_expected_grant_revision + 1 = revision),
    CHECK (provenance_json IS NULL OR owner_kind = 'user'),
    CHECK (updated_at >= created_at)
);

CREATE TABLE auth_sessions (
    session_id TEXT PRIMARY KEY CHECK (length(session_id) = 26),
    principal_id TEXT NOT NULL REFERENCES auth_principals(principal_id),
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    participant_kind TEXT NOT NULL CHECK (participant_kind IN ('app', 'agent')),
    session_public_key TEXT NOT NULL CHECK (length(session_public_key) = 43),
    session_key_id TEXT NOT NULL CHECK (length(session_key_id) = 43),
    state TEXT NOT NULL CHECK (state IN ('active', 'expired', 'revoked')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    last_authenticated_at INTEGER NOT NULL CHECK (last_authenticated_at BETWEEN 0 AND 9007199254740991),
    expires_at INTEGER CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    revoked_at INTEGER CHECK (revoked_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    UNIQUE (participant_id, session_public_key),
    UNIQUE (participant_id, session_key_id),
    CHECK (last_authenticated_at >= created_at),
    CHECK (expires_at IS NULL OR expires_at >= created_at),
    CHECK ((state = 'revoked') = (revoked_at IS NOT NULL))
);
CREATE INDEX auth_sessions_principal_idx ON auth_sessions(principal_id);

CREATE TRIGGER auth_sessions_immutable_identity BEFORE UPDATE ON auth_sessions
WHEN NEW.session_id IS NOT OLD.session_id OR NEW.principal_id IS NOT OLD.principal_id
  OR NEW.participant_id IS NOT OLD.participant_id OR NEW.participant_kind IS NOT OLD.participant_kind
  OR NEW.session_public_key IS NOT OLD.session_public_key OR NEW.session_key_id IS NOT OLD.session_key_id
  OR NEW.created_at IS NOT OLD.created_at OR NEW.expires_at IS NOT OLD.expires_at
BEGIN SELECT RAISE(ABORT, 'login identity and absolute expiry are immutable'); END;
CREATE TRIGGER auth_sessions_no_revival BEFORE UPDATE ON auth_sessions
WHEN (OLD.state != 'active' AND NEW.state = 'active')
  OR (OLD.state = 'revoked' AND (NEW.state != 'revoked' OR NEW.revoked_at IS NOT OLD.revoked_at))
BEGIN SELECT RAISE(ABORT, 'terminal logins cannot be revived'); END;
CREATE TRIGGER auth_sessions_no_delete BEFORE DELETE ON auth_sessions
BEGIN SELECT RAISE(ABORT, 'login tombstones are retained'); END;

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

CREATE TABLE auth_device_delegations (
    principal_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    required INTEGER NOT NULL CHECK (required IN (0, 1)),
    companion_participant_id TEXT,
    user_login_session_id TEXT REFERENCES auth_sessions(session_id),
    installation_public_key TEXT,
    device_grant_revision INTEGER CHECK (device_grant_revision BETWEEN 1 AND 9007199254740991),
    child_grant_revision INTEGER CHECK (child_grant_revision BETWEEN 1 AND 9007199254740991),
    state TEXT NOT NULL CHECK (state IN ('active', 'missing', 'revoked')),
    expires_at INTEGER CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY (principal_id, deployment_id),
    FOREIGN KEY (principal_id, deployment_id)
        REFERENCES auth_devices(principal_id, deployment_id) ON DELETE CASCADE,
    CHECK (companion_participant_id IS NULL OR length(companion_participant_id) > 0),
    CHECK (installation_public_key IS NULL OR length(installation_public_key) = 43),
    CHECK ((companion_participant_id IS NULL) = (user_login_session_id IS NULL)),
    CHECK ((companion_participant_id IS NULL) = (installation_public_key IS NULL)),
    CHECK ((companion_participant_id IS NULL) = (device_grant_revision IS NULL)),
    CHECK ((companion_participant_id IS NULL) = (child_grant_revision IS NULL)),
    CHECK (state != 'active' OR companion_participant_id IS NOT NULL)
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

CREATE TABLE auth_login_portals (
    portal_id TEXT PRIMARY KEY CHECK (length(portal_id) > 0),
    display_name TEXT NOT NULL CHECK (length(display_name) > 0),
    entry_url TEXT,
    builtin INTEGER NOT NULL CHECK (builtin IN (0, 1)),
    disabled INTEGER NOT NULL CHECK (disabled IN (0, 1)),
    removed INTEGER NOT NULL CHECK (removed IN (0, 1)),
    local_registration_enabled INTEGER NOT NULL CHECK (local_registration_enabled IN (0, 1)),
    provider_ids_json TEXT NOT NULL CHECK (json_valid(provider_ids_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991)
);

CREATE TABLE auth_login_settings (
    portal_id TEXT PRIMARY KEY REFERENCES auth_login_portals(portal_id) ON DELETE CASCADE,
    default_provider_id TEXT,
    local_login_enabled INTEGER NOT NULL CHECK (local_login_enabled IN (0, 1)),
    federated_registration_enabled INTEGER NOT NULL CHECK (federated_registration_enabled IN (0, 1)),
    provider_selection_enabled INTEGER NOT NULL CHECK (provider_selection_enabled IN (0, 1)),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991)
);

CREATE TABLE auth_deployment_profiles (
    deployment_id TEXT PRIMARY KEY REFERENCES auth_principals(principal_id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('service', 'device')),
    display_name TEXT NOT NULL CHECK (length(display_name) > 0),
    participant_id TEXT,
    portal_id TEXT REFERENCES auth_login_portals(portal_id) ON DELETE SET NULL,
    requires_device_delegation INTEGER NOT NULL CHECK (requires_device_delegation IN (0, 1)),
    expires_at INTEGER CHECK (expires_at IS NULL OR expires_at BETWEEN 0 AND 9007199254740991),
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled', 'removed')),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    review_mode TEXT,
    CHECK ((kind = 'device' AND review_mode IN ('none', 'required'))
        OR (kind = 'service' AND review_mode IS NULL))
);

CREATE TABLE auth_portal_routes (
    route_id TEXT PRIMARY KEY CHECK (length(route_id) > 0),
    portal_id TEXT NOT NULL REFERENCES auth_login_portals(portal_id) ON DELETE CASCADE,
    participant_id TEXT,
    origin TEXT,
    deployment_id TEXT REFERENCES auth_deployments(deployment_id) ON DELETE CASCADE,
    priority INTEGER NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    CHECK (updated_at >= created_at),
    CHECK (participant_id IS NOT NULL OR origin IS NOT NULL OR deployment_id IS NOT NULL)
);
CREATE INDEX auth_portal_routes_selection_idx
    ON auth_portal_routes(priority DESC, route_id);

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
    live_until_seconds INTEGER NOT NULL DEFAULT 0 CHECK (live_until_seconds >= 0),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    revoked_at INTEGER CHECK (revoked_at BETWEEN 0 AND 9007199254740991),
    CHECK (is_current = 0 OR revoked_at IS NULL)
);
CREATE UNIQUE INDEX idx_auth_authorization_current_issuer
    ON auth_authorization_issuers(is_current) WHERE is_current = 1;
CREATE TRIGGER auth_authorization_issuer_revocation_is_final
BEFORE UPDATE OF revoked_at ON auth_authorization_issuers
WHEN OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS NOT OLD.revoked_at
BEGIN
    SELECT RAISE(ABORT, 'issuer revocation is irreversible');
END;

CREATE TABLE auth_authorization_contexts (
    context_digest TEXT PRIMARY KEY CHECK (length(context_digest) = 43),
    connection_id TEXT NOT NULL CHECK (length(connection_id) = 26),
    session_public_key TEXT NOT NULL CHECK (length(session_public_key) = 43),
    inbox_prefix TEXT NOT NULL CHECK (length(inbox_prefix) > 0),
    principal_id TEXT NOT NULL CHECK (length(principal_id) > 0),
    principal_kind TEXT NOT NULL CHECK (principal_kind IN ('user', 'service', 'device')),
    participant_id TEXT NOT NULL CHECK (length(participant_id) > 0),
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('user', 'deployment')),
    owner_id TEXT NOT NULL CHECK (length(owner_id) > 0),
    grant_revision INTEGER NOT NULL CHECK (grant_revision BETWEEN 1 AND 9007199254740991),
    installed_revision INTEGER NOT NULL CHECK (installed_revision BETWEEN 1 AND 9007199254740991),
    identity_key_id TEXT,
    login_session_id TEXT,
    issuer_key_id TEXT NOT NULL CHECK (length(issuer_key_id) = 43),
    signed_context_json TEXT NOT NULL CHECK (json_valid(signed_context_json)),
    issuance_snapshot_token TEXT NOT NULL CHECK (length(issuance_snapshot_token) = 43),
    issued_at INTEGER NOT NULL CHECK (issued_at BETWEEN 0 AND 9007199254740991),
    not_before INTEGER NOT NULL CHECK (not_before BETWEEN 0 AND 9007199254740991),
    refresh_at INTEGER NOT NULL CHECK (refresh_at BETWEEN 0 AND 9007199254740991),
    expires_at INTEGER NOT NULL CHECK (expires_at BETWEEN 0 AND 9007199254740991),
    state TEXT NOT NULL CHECK (state IN ('active', 'revoked', 'expired')),
    published_at INTEGER CHECK (published_at BETWEEN 0 AND 9007199254740991),
    revoked_at INTEGER CHECK (revoked_at BETWEEN 0 AND 9007199254740991),
    revocation_reason TEXT,
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    FOREIGN KEY (participant_id, installed_revision)
        REFERENCES auth_installed_participants(participant_id, revision),
    CHECK ((principal_kind = 'user' AND owner_kind = 'user' AND owner_id = principal_id
            AND login_session_id IS NOT NULL AND identity_key_id IS NULL)
        OR (principal_kind IN ('service', 'device') AND owner_kind = 'deployment'
            AND identity_key_id IS NOT NULL AND login_session_id IS NULL)),
    CHECK (login_session_id IS NULL OR length(login_session_id) = 26),
    CHECK (identity_key_id IS NULL OR length(identity_key_id) = 43),
    CHECK (issued_at <= refresh_at),
    CHECK (not_before <= expires_at),
    CHECK (refresh_at <= expires_at),
    CHECK ((state = 'revoked') = (revoked_at IS NOT NULL)),
    CHECK ((state = 'revoked') = (revocation_reason IS NOT NULL)),
    CHECK (state = 'revoked' OR (revoked_at IS NULL AND revocation_reason IS NULL))
);
CREATE TRIGGER auth_authorization_context_history_no_delete
BEFORE DELETE ON auth_authorization_contexts
BEGIN
    SELECT RAISE(ABORT, 'authorization context history cannot be deleted');
END;
CREATE TRIGGER auth_authorization_context_evidence_is_immutable
BEFORE UPDATE OF context_digest, connection_id, session_public_key, inbox_prefix, principal_id, principal_kind,
    participant_id, owner_kind, owner_id, grant_revision, installed_revision,
    identity_key_id, login_session_id, issuer_key_id, signed_context_json,
    issuance_snapshot_token, issued_at, not_before, refresh_at, expires_at
ON auth_authorization_contexts
BEGIN
    SELECT RAISE(ABORT, 'issued authorization evidence is immutable');
END;
CREATE TRIGGER auth_authorization_context_revocation_is_final
BEFORE UPDATE OF state, revoked_at, revocation_reason ON auth_authorization_contexts
WHEN OLD.state = 'revoked' AND (
    NEW.state IS NOT OLD.state OR NEW.revoked_at IS NOT OLD.revoked_at
    OR NEW.revocation_reason IS NOT OLD.revocation_reason
)
BEGIN
    SELECT RAISE(ABORT, 'context revocation is irreversible');
END;
CREATE INDEX auth_authorization_contexts_connection_idx
    ON auth_authorization_contexts(connection_id, state, expires_at);
CREATE INDEX auth_authorization_contexts_login_idx
    ON auth_authorization_contexts(login_session_id, state, expires_at);
CREATE INDEX auth_authorization_contexts_principal_idx
    ON auth_authorization_contexts(principal_id, state, expires_at);
CREATE INDEX auth_authorization_contexts_grant_idx
    ON auth_authorization_contexts(owner_kind, owner_id, participant_id, state, expires_at);
CREATE INDEX auth_authorization_contexts_identity_idx
    ON auth_authorization_contexts(identity_key_id, state, expires_at);
CREATE INDEX auth_authorization_contexts_issuer_idx
    ON auth_authorization_contexts(issuer_key_id, state, expires_at);
CREATE INDEX auth_authorization_contexts_state_idx
    ON auth_authorization_contexts(state, expires_at);

CREATE TABLE auth_post_commit_actions (
    action_id TEXT PRIMARY KEY CHECK (length(action_id) = 43),
    kind TEXT NOT NULL CHECK (kind IN ('event', 'kick', 'context_publish', 'context_revoke', 'resource_reconcile')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at BETWEEN 0 AND 9007199254740991),
    claimed_until INTEGER CHECK (claimed_until BETWEEN 0 AND 9007199254740991),
    claim_token TEXT CHECK (claim_token IS NULL OR length(claim_token) = 26),
    last_error TEXT,
    predecessor_action_id TEXT,
    event_delivery_json TEXT CHECK (event_delivery_json IS NULL OR json_valid(event_delivery_json))
);
CREATE INDEX auth_post_commit_actions_ready_idx
    ON auth_post_commit_actions(next_attempt_at, action_id);
CREATE INDEX auth_post_commit_actions_predecessor
    ON auth_post_commit_actions(predecessor_action_id);

CREATE TABLE auth_capability_groups (
    group_key TEXT PRIMARY KEY CHECK (length(group_key) > 0),
    display_name TEXT NOT NULL CHECK (length(display_name) > 0),
    description TEXT NOT NULL CHECK (length(description) > 0),
    capabilities_json TEXT NOT NULL CHECK (json_valid(capabilities_json)),
    included_groups_json TEXT NOT NULL CHECK (json_valid(included_groups_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991)
);
CREATE TABLE auth_portal_grant_overrides (
    portal_id TEXT NOT NULL REFERENCES auth_login_portals(portal_id) ON DELETE CASCADE,
    participant_id TEXT NOT NULL,
    direct_capabilities_json TEXT NOT NULL CHECK (json_valid(direct_capabilities_json)),
    capability_group_keys_json TEXT NOT NULL CHECK (json_valid(capability_group_keys_json)),
    role_mappings_json TEXT NOT NULL CHECK (json_valid(role_mappings_json)),
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK (updated_at BETWEEN 0 AND 9007199254740991),
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 9007199254740991),
    PRIMARY KEY (portal_id, participant_id)
);

CREATE TABLE auth_bootstrap_administrator (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    principal_id TEXT NOT NULL UNIQUE REFERENCES auth_principals(principal_id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL CHECK (created_at BETWEEN 0 AND 9007199254740991)
);
