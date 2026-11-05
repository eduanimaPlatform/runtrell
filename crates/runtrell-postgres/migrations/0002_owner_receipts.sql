-- Additive schema-4 extension. Existing migration bytes remain unchanged.
CREATE TABLE IF NOT EXISTS {{schema}}.owner_receipts (
    key text PRIMARY KEY,
    receipt jsonb NOT NULL,
    CHECK (key = receipt->'command'->>'key')
);
-- The runtime only INSERTs and SELECTs receipts. Refuse accidental DML rewrites,
-- including same-value updates; the schema owner retains ordinary DDL authority.
CREATE OR REPLACE FUNCTION {{schema}}.immutable_owner_receipt() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
    RAISE EXCEPTION 'owner receipts are immutable' USING ERRCODE = '55000';
END $$;
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgrelid = '{{schema}}.owner_receipts'::regclass
                   AND tgname = 'immutable_owner_receipt') THEN
        CREATE TRIGGER immutable_owner_receipt BEFORE UPDATE OR DELETE ON {{schema}}.owner_receipts
        FOR EACH ROW EXECUTE FUNCTION {{schema}}.immutable_owner_receipt();
    END IF;
END $$;
