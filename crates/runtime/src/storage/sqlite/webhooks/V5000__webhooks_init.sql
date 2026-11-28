-- One record per request to a known webhook endpoint. Records never hold
-- header values, bodies, secrets, or signatures.
CREATE TABLE webhook_attempts (
    attempt_id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL CHECK (length(deployment_id) > 0),
    endpoint TEXT NOT NULL CHECK (length(endpoint) > 0),
    received_at INTEGER NOT NULL CHECK (received_at >= 0),
    client_address TEXT NOT NULL,
    method TEXT NOT NULL,
    body_size INTEGER NOT NULL CHECK (body_size >= 0),
    body_sha256 TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('accepted', 'ignored', 'duplicate', 'pending',
        'rejected', 'limited', 'unavailable', 'invalid_decision', 'expired')),
    reason TEXT,
    matched_key TEXT CHECK (matched_key IN ('current', 'previous')),
    verdict TEXT CHECK (verdict IN ('accept', 'ignore', 'reject')),
    -- 1 once the request's verifiers reached a verdict on it; 0 for a request
    -- refused before verification (method, size, encoding, rate limits) or
    -- still waiting for it. Endpoint health reads only verdicts.
    reached_verification INTEGER NOT NULL CHECK (reached_verification IN (0, 1)),
    status INTEGER,
    delivery_id TEXT,
    -- The request's sequence in its deployment's intake stream, when stored there.
    stream_sequence INTEGER,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);
CREATE INDEX webhook_attempts_by_endpoint
    ON webhook_attempts(deployment_id, endpoint, attempt_id);
CREATE INDEX webhook_attempts_by_outcome ON webhook_attempts(outcome, received_at);
-- When a request last matched each value of a secret, so administrators see
-- when a sender has switched. A match belongs to the secret it used, the
-- generation of that secret the request arrived under, and whether it
-- matched that generation's value or the one it replaced; a later change
-- starts afresh.
CREATE TABLE webhook_secret_matches (
    deployment_id TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    field TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    matched_key TEXT NOT NULL CHECK (matched_key IN ('current', 'previous')),
    matched_at INTEGER NOT NULL CHECK (matched_at >= 0),
    PRIMARY KEY (deployment_id, endpoint, field, generation, matched_key)
);
