-- Schema 3 is a fresh schema; no upgrade of prior schemas.
CREATE TABLE IF NOT EXISTS {{schema}}.schema_config (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    version integer NOT NULL CHECK (version = 3), capacity jsonb NOT NULL
);
CREATE TABLE IF NOT EXISTS {{schema}}.runs (
    handle jsonb NOT NULL,
    tenant text GENERATED ALWAYS AS (handle->>'tenant') STORED NOT NULL,
    id text GENERATED ALWAYS AS (handle->>'id') STORED NOT NULL,
    spec jsonb NOT NULL,
    principal text GENERATED ALWAYS AS (spec->>'principal') STORED NOT NULL,
    request_key text GENERATED ALWAYS AS (spec->>'request_key') STORED NOT NULL,
    account text GENERATED ALWAYS AS (spec->>'account') STORED NOT NULL,
    session_id text GENERATED ALWAYS AS (spec->>'session') STORED,
    parent_tenant text GENERATED ALWAYS AS (spec->'parent'->>'tenant') STORED,
    parent_id text GENERATED ALWAYS AS (spec->'parent'->>'id') STORED,
    schema integer NOT NULL, revision numeric(20,0) NOT NULL,
    sequence numeric(20,0) NOT NULL, generation numeric(20,0) NOT NULL,
    owner jsonb,
    owner_worker text GENERATED ALWAYS AS (owner->>'worker') STORED,
    owner_generation numeric(20,0) GENERATED ALWAYS AS ((owner->>'generation')::numeric) STORED,
    lease_until numeric(20,0) GENERATED ALWAYS AS ((owner->>'lease_until')::numeric) STORED,
    state jsonb NOT NULL, checkpoint jsonb, cancel text,
    stop jsonb NOT NULL, cleanup jsonb NOT NULL, settlement jsonb NOT NULL,
    delivery jsonb NOT NULL, output jsonb, receipt jsonb, completion text,
    PRIMARY KEY(tenant,id), UNIQUE(tenant,principal,request_key)
);
CREATE INDEX IF NOT EXISTS run_parent ON {{schema}}.runs(parent_tenant,parent_id);
CREATE INDEX IF NOT EXISTS session_pending ON {{schema}}.runs(tenant,session_id,sequence)
    WHERE NOT (state ? 'Terminal');
CREATE TABLE IF NOT EXISTS {{schema}}.sessions (
    key text PRIMARY KEY, sequence numeric(20,0) NOT NULL DEFAULT 0,
    context_revision numeric(20,0) NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS {{schema}}.budget_accounts (
    id text PRIMARY KEY, tenant text NOT NULL,
    parent text REFERENCES {{schema}}.budget_accounts(id),
    "limit" jsonb NOT NULL, reserved jsonb NOT NULL, used jsonb NOT NULL,
    unknown numeric(20,0) NOT NULL, exhausted boolean NOT NULL, warnings jsonb NOT NULL, policy jsonb NOT NULL, quantity_reserved jsonb NOT NULL, quantity_used jsonb NOT NULL, quantity_warnings jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS account_parent ON {{schema}}.budget_accounts(parent);
CREATE TABLE IF NOT EXISTS {{schema}}.operations (
    tenant text NOT NULL, run_id text NOT NULL, key text NOT NULL,
    PRIMARY KEY(tenant,run_id,key),
    FOREIGN KEY(tenant,run_id) REFERENCES {{schema}}.runs(tenant,id)
);
CREATE TABLE IF NOT EXISTS {{schema}}.attempts (
    id text PRIMARY KEY, run jsonb NOT NULL,
    tenant text GENERATED ALWAYS AS (run->>'tenant') STORED NOT NULL,
    run_id text GENERATED ALWAYS AS (run->>'id') STORED NOT NULL,
    request jsonb NOT NULL,
    operation_key text GENERATED ALWAYS AS (request->>'key') STORED NOT NULL,
    attempt bigint GENERATED ALWAYS AS ((request->>'attempt')::bigint) STORED NOT NULL,
    state jsonb NOT NULL, result jsonb,
    external_id text GENERATED ALWAYS AS
      (COALESCE(result->'Applied'->>'external_id',result->'Accepted'->>'external_id')) STORED,
    settled boolean NOT NULL, unknown_usage boolean NOT NULL,
    send_generation numeric(20,0), send_count bigint NOT NULL,
    replay_proof_generation numeric(20,0), created_at numeric(20,0) NOT NULL,
    finished_at numeric(20,0), usage_observations jsonb NOT NULL, unsettled_result jsonb,
    UNIQUE(tenant,run_id,operation_key,attempt),
    FOREIGN KEY(tenant,run_id,operation_key) REFERENCES {{schema}}.operations(tenant,run_id,key)
);
CREATE TABLE IF NOT EXISTS {{schema}}.budget_entries (
    attempt_id text REFERENCES {{schema}}.attempts(id),
    account_id text REFERENCES {{schema}}.budget_accounts(id),
    reservation jsonb NOT NULL, usage jsonb, unknown boolean NOT NULL, settled boolean NOT NULL,
    PRIMARY KEY(attempt_id,account_id)
);
CREATE INDEX IF NOT EXISTS budget_unsettled ON {{schema}}.budget_entries(account_id) WHERE NOT settled;
CREATE TABLE IF NOT EXISTS {{schema}}.slots (
    key text PRIMARY KEY, tenant text NOT NULL, run text NOT NULL, kind jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS slot_run ON {{schema}}.slots(tenant,run,kind);
CREATE TABLE IF NOT EXISTS {{schema}}.queue_tenants (
    tenant text PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS {{schema}}.queue_entries (
    key text PRIMARY KEY, tenant text NOT NULL REFERENCES {{schema}}.queue_tenants(tenant),
    position bigserial UNIQUE, scheduling jsonb NOT NULL, deadline numeric(20,0) NOT NULL
);
CREATE INDEX IF NOT EXISTS queue_tenant_order ON {{schema}}.queue_entries(tenant,position);
CREATE INDEX IF NOT EXISTS queue_deadline ON {{schema}}.queue_entries(deadline,position);
-- Each permit is an independent lockable row, not a shared counter.
CREATE TABLE IF NOT EXISTS {{schema}}.capacity_slots (
    scope text NOT NULL, ordinal bigint NOT NULL, holder text,
    PRIMARY KEY(scope,ordinal), UNIQUE(scope,holder)
);
CREATE INDEX IF NOT EXISTS capacity_free ON {{schema}}.capacity_slots(scope,ordinal) WHERE holder IS NULL;
CREATE INDEX IF NOT EXISTS capacity_holder ON {{schema}}.capacity_slots(holder) WHERE holder IS NOT NULL;
CREATE TABLE IF NOT EXISTS {{schema}}.late_results (
    operation text NOT NULL REFERENCES {{schema}}.attempts(id), generation text NOT NULL,
    worker text NOT NULL, digest text NOT NULL, observed_at numeric(20,0) NOT NULL,
    result jsonb NOT NULL, PRIMARY KEY(operation,generation,worker,digest)
);
-- Dispatch ordering metadata only; never acquired by Run/budget/slot updates.
CREATE TABLE IF NOT EXISTS {{schema}}.queue_policy (
    singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton),
    last_tenant text, priority_streak integer NOT NULL DEFAULT 0
);
INSERT INTO {{schema}}.queue_policy(singleton) VALUES(true) ON CONFLICT DO NOTHING;
CREATE INDEX IF NOT EXISTS attempts_unsettled ON {{schema}}.attempts(tenant,run_id) WHERE NOT settled;
CREATE INDEX IF NOT EXISTS slot_kind ON {{schema}}.slots(kind);
-- Independent execution inventory: fixed inputs plus monotonic cleanup receipts.
CREATE TABLE IF NOT EXISTS {{schema}}.executions (
    owner jsonb NOT NULL,
    tenant text GENERATED ALWAYS AS (owner->>'tenant') STORED NOT NULL,
    run_id text GENERATED ALWAYS AS (owner->>'id') STORED NOT NULL,
    context_operation text NOT NULL,
    request jsonb NOT NULL,
    revision numeric(20,0) NOT NULL CHECK (revision >= 0),
    cancel_requested boolean NOT NULL,
    observation jsonb,
    resources jsonb NOT NULL,
    reclaimed boolean NOT NULL,
    PRIMARY KEY(tenant,run_id,context_operation)
);
CREATE INDEX IF NOT EXISTS execution_pending ON {{schema}}.executions(tenant,run_id,context_operation)
    WHERE NOT reclaimed;
