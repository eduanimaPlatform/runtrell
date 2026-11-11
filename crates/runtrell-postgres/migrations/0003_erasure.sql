-- Additive schema-4 erasure fence. Kept separate from the original schema.
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = '{{schema}}.runs'::regclass
                   AND attname = 'erasure' AND NOT attisdropped) THEN
        ALTER TABLE {{schema}}.runs ADD COLUMN erasure text
            CHECK (erasure IN ('pending', 'done'));
    END IF;
END $$;

-- Only opaque SVG provider JSON is redacted from immutable owner accounting
-- receipts. Normalized counters, model, price, amount and basis remain intact.
CREATE OR REPLACE FUNCTION {{schema}}.erase_svg_reported(svg jsonb) RETURNS jsonb
LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    reported jsonb := svg->'reported';
    cleaned jsonb;
    field text;
    counter jsonb;
BEGIN
    IF jsonb_typeof(reported) IS DISTINCT FROM 'object' THEN
        RETURN 'null'::jsonb;
    END IF;
    cleaned := jsonb_build_object('model', CASE
        WHEN jsonb_typeof(svg->'model') = 'string' AND reported->'model' = svg->'model'
        THEN svg->'model' ELSE 'false'::jsonb END);
    FOREACH field IN ARRAY ARRAY['input_tokens','output_tokens','reasoning_tokens','total_tokens'] LOOP
        counter := reported->field;
        IF counter IS NULL OR counter = 'null'::jsonb THEN
            counter := 'null'::jsonb;
        ELSIF jsonb_typeof(counter) = 'number'
              AND counter::text ~ '^(0|[1-9][0-9]*)$' THEN
            IF counter::text::numeric > 18446744073709551615 THEN
                counter := 'false'::jsonb;
            END IF; -- Retain valid u64 numbers, including normalized-counter mismatches.
        ELSE
            counter := 'false'::jsonb; -- Preserve invalid evidence without retaining its text.
        END IF;
        cleaned := cleaned || jsonb_build_object(field, counter);
    END LOOP;
    RETURN cleaned;
END $$;

CREATE OR REPLACE FUNCTION {{schema}}.erase_receipt_payload(receipt jsonb) RETURNS jsonb
LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    cleaned jsonb := receipt;
    usage_path text[];
    svg jsonb;
BEGIN
    FOR usage_path IN SELECT path FROM (VALUES
        (ARRAY['request','Settle','recovery','usage','svg_usage']),
        (ARRAY['result','Settled','previous','svg_usage']),
        (ARRAY['result','Settled','settled','svg_usage'])
    ) AS paths(path) LOOP
        svg := cleaned #> usage_path;
        IF jsonb_typeof(svg) = 'object' THEN
            cleaned := jsonb_set(cleaned, array_append(usage_path, 'reported'), {{schema}}.erase_svg_reported(svg));
        END IF;
    END LOOP;
    RETURN cleaned;
END $$;

-- The sole receipt UPDATE permitted is the exact deterministic redaction above,
-- while its owning Run is fenced pending erasure. Same-value and other DML still
-- fail, including DELETE. The original immutable trigger continues to call this
-- replaced function.
CREATE OR REPLACE FUNCTION {{schema}}.immutable_owner_receipt() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.key = OLD.key
           AND NEW.receipt = {{schema}}.erase_receipt_payload(OLD.receipt)
           AND NEW.receipt <> OLD.receipt
           AND EXISTS (
               SELECT 1 FROM {{schema}}.runs r
               WHERE r.tenant = OLD.receipt->'run'->>'tenant'
                 AND r.id = OLD.receipt->'run'->>'id'
                 AND r.erasure = 'pending'
           ) THEN
            RETURN NEW;
        END IF;
    END IF;
    RAISE EXCEPTION 'owner receipts are immutable' USING ERRCODE = '55000';
END $$;
