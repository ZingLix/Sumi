-- Run as a migration owner, never as the HTTP runtime role.
-- Unconstrained NUMERIC plus checks avoids NUMERIC(38,0)'s implicit rounding
-- before a CHECK can reject a fractional input.
CREATE DOMAIN minor_amount AS numeric
    CHECK (VALUE = trunc(VALUE) AND abs(VALUE) <= 99999999999999999999999999999999999999);

CREATE TABLE users (
    id uuid PRIMARY KEY,
    issuer text NOT NULL,
    subject text NOT NULL,
    UNIQUE (issuer, subject)
);
CREATE TABLE ledgers (
    id uuid PRIMARY KEY,
    owner_user_id uuid NOT NULL UNIQUE REFERENCES users(id),
    reporting_currency text NOT NULL DEFAULT 'CNY',
    timezone text NOT NULL DEFAULT 'Asia/Shanghai',
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0)
);
CREATE TABLE accounts (
    ledger_id uuid NOT NULL REFERENCES ledgers(id),
    id uuid NOT NULL,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 256),
    kind text NOT NULL CHECK (kind IN ('asset','liability','income','expense','equity')),
    currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    role text CHECK (role IN ('income','expense','opening','suspense','transit','exchange')),
    active boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    PRIMARY KEY (ledger_id,id),
    UNIQUE (ledger_id,id,currency),
    UNIQUE (ledger_id,role,currency)
);
CREATE TABLE operations (
    ledger_id uuid NOT NULL REFERENCES ledgers(id),
    id uuid NOT NULL,
    user_id uuid NOT NULL REFERENCES users(id),
    client_id text NOT NULL,
    tool text NOT NULL,
    idempotency_key text NOT NULL,
    request_hash text NOT NULL,
    request jsonb NOT NULL,
    result jsonb NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (ledger_id,id),
    UNIQUE (ledger_id,client_id,tool,idempotency_key)
);
CREATE TABLE transactions (
    ledger_id uuid NOT NULL REFERENCES ledgers(id),
    id uuid NOT NULL,
    current_version bigint NOT NULL CHECK (current_version > 0),
    state text NOT NULL CHECK (state IN ('posted','voided')),
    opening_account_id uuid,
    PRIMARY KEY (ledger_id,id),
    UNIQUE (ledger_id,opening_account_id),
    FOREIGN KEY (ledger_id,opening_account_id) REFERENCES accounts(ledger_id,id)
);
CREATE TABLE transaction_versions (
    ledger_id uuid NOT NULL,
    transaction_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    data jsonb NOT NULL,
    state text NOT NULL CHECK (state IN ('posted','voided')),
    reason text NOT NULL,
    operation_id uuid NOT NULL,
    recorded_revision bigint NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (ledger_id,transaction_id,version),
    FOREIGN KEY (ledger_id,transaction_id) REFERENCES transactions(ledger_id,id),
    FOREIGN KEY (ledger_id,operation_id) REFERENCES operations(ledger_id,id) DEFERRABLE INITIALLY DEFERRED
);
ALTER TABLE transactions ADD FOREIGN KEY (ledger_id,id,current_version)
    REFERENCES transaction_versions(ledger_id,transaction_id,version) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE journal_entries (
    ledger_id uuid NOT NULL,
    id uuid NOT NULL,
    transaction_id uuid NOT NULL,
    transaction_version bigint NOT NULL,
    effective_on date NOT NULL,
    recorded_revision bigint NOT NULL,
    operation_id uuid NOT NULL,
    reverses_entry_id uuid,
    created_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    PRIMARY KEY (ledger_id,id),
    UNIQUE (ledger_id,reverses_entry_id),
    FOREIGN KEY (ledger_id,transaction_id,transaction_version)
        REFERENCES transaction_versions(ledger_id,transaction_id,version),
    FOREIGN KEY (ledger_id,operation_id) REFERENCES operations(ledger_id,id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (ledger_id,reverses_entry_id) REFERENCES journal_entries(ledger_id,id),
    CHECK (reverses_entry_id IS DISTINCT FROM id)
);
CREATE TABLE journal_lines (
    ledger_id uuid NOT NULL,
    entry_id uuid NOT NULL,
    line_number integer NOT NULL CHECK (line_number > 0),
    account_id uuid NOT NULL,
    currency text NOT NULL,
    amount_minor minor_amount NOT NULL CHECK (amount_minor <> 0),
    PRIMARY KEY (ledger_id,entry_id,line_number),
    FOREIGN KEY (ledger_id,entry_id) REFERENCES journal_entries(ledger_id,id),
    FOREIGN KEY (ledger_id,account_id,currency) REFERENCES accounts(ledger_id,id,currency)
);
CREATE INDEX journal_lines_account ON journal_lines(ledger_id,account_id,entry_id);
CREATE INDEX journal_entries_date ON journal_entries(ledger_id,effective_on,id);
CREATE INDEX transaction_versions_operation ON transaction_versions(ledger_id,operation_id);

CREATE FUNCTION reject_history_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'History is append-only' USING ERRCODE = '23514'; END;
$$;
CREATE TRIGGER immutable_entries BEFORE UPDATE OR DELETE ON journal_entries FOR EACH ROW EXECUTE FUNCTION reject_history_mutation();
CREATE TRIGGER immutable_lines BEFORE UPDATE OR DELETE ON journal_lines FOR EACH ROW EXECUTE FUNCTION reject_history_mutation();
CREATE TRIGGER immutable_versions BEFORE UPDATE OR DELETE ON transaction_versions FOR EACH ROW EXECUTE FUNCTION reject_history_mutation();
CREATE TRIGGER immutable_operations BEFORE UPDATE OR DELETE ON operations FOR EACH ROW EXECUTE FUNCTION reject_history_mutation();

CREATE FUNCTION protect_sealed_entry() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS(SELECT 1 FROM journal_entries WHERE ledger_id=NEW.ledger_id AND id=NEW.entry_id AND created_xid=pg_current_xact_id()) THEN
        RAISE EXCEPTION 'Cannot append lines to a committed journal entry' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER sealed_entry BEFORE INSERT ON journal_lines FOR EACH ROW EXECUTE FUNCTION protect_sealed_entry();

CREATE FUNCTION validate_journal_entry() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    entry_uuid uuid;
    entry_row journal_entries;
    old_row journal_entries;
BEGIN
    IF TG_TABLE_NAME = 'journal_entries' THEN entry_uuid := NEW.id;
    ELSE entry_uuid := NEW.entry_id; END IF;
    SELECT * INTO STRICT entry_row FROM journal_entries WHERE ledger_id = NEW.ledger_id AND id = entry_uuid;
    IF (SELECT count(*) FROM journal_lines WHERE ledger_id = NEW.ledger_id AND entry_id = entry_uuid) < 2
       OR EXISTS (SELECT 1 FROM journal_lines WHERE ledger_id = NEW.ledger_id AND entry_id = entry_uuid
                  GROUP BY currency HAVING sum(amount_minor) <> 0) THEN
        RAISE EXCEPTION 'Journal entry must have two nonzero lines and balance per currency' USING ERRCODE = '23514';
    END IF;
    IF entry_row.reverses_entry_id IS NOT NULL THEN
        SELECT * INTO STRICT old_row FROM journal_entries WHERE ledger_id = NEW.ledger_id AND id = entry_row.reverses_entry_id;
        IF old_row.reverses_entry_id IS NOT NULL OR old_row.transaction_id <> entry_row.transaction_id
           OR old_row.effective_on <> entry_row.effective_on THEN
            RAISE EXCEPTION 'Invalid reversal target or date' USING ERRCODE = '23514';
        END IF;
        IF EXISTS (
            (SELECT account_id,currency,amount_minor FROM journal_lines WHERE ledger_id=NEW.ledger_id AND entry_id=entry_uuid
             EXCEPT ALL SELECT account_id,currency,-amount_minor FROM journal_lines WHERE ledger_id=NEW.ledger_id AND entry_id=old_row.id)
            UNION ALL
            (SELECT account_id,currency,-amount_minor FROM journal_lines WHERE ledger_id=NEW.ledger_id AND entry_id=old_row.id
             EXCEPT ALL SELECT account_id,currency,amount_minor FROM journal_lines WHERE ledger_id=NEW.ledger_id AND entry_id=entry_uuid)
        ) THEN RAISE EXCEPTION 'Reversal must exactly negate the original entry' USING ERRCODE = '23514'; END IF;
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER balanced_entry AFTER INSERT ON journal_entries DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION validate_journal_entry();
CREATE CONSTRAINT TRIGGER balanced_line AFTER INSERT ON journal_lines DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION validate_journal_entry();

CREATE FUNCTION protect_account_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.ledger_id <> OLD.ledger_id OR NEW.id <> OLD.id OR NEW.role IS DISTINCT FROM OLD.role THEN
        RAISE EXCEPTION 'Account identity is immutable' USING ERRCODE='23514';
    END IF;
    IF (NEW.kind <> OLD.kind OR NEW.currency <> OLD.currency) AND
       (EXISTS (SELECT 1 FROM journal_lines WHERE ledger_id=OLD.ledger_id AND account_id=OLD.id)
        OR EXISTS (SELECT 1 FROM transactions WHERE ledger_id=OLD.ledger_id AND opening_account_id=OLD.id)) THEN
        RAISE EXCEPTION 'Posted account type and currency are immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER account_identity BEFORE UPDATE ON accounts FOR EACH ROW EXECUTE FUNCTION protect_account_identity();

ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE users FORCE ROW LEVEL SECURITY;
CREATE POLICY self ON users USING (id = nullif(current_setting('sumi.user_id',true),'')::uuid)
    WITH CHECK (id = nullif(current_setting('sumi.user_id',true),'')::uuid);
ALTER TABLE ledgers ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledgers FORCE ROW LEVEL SECURITY;
CREATE POLICY owned ON ledgers USING (owner_user_id = nullif(current_setting('sumi.user_id',true),'')::uuid)
    WITH CHECK (owner_user_id = nullif(current_setting('sumi.user_id',true),'')::uuid);
DO $$ DECLARE t text; BEGIN
    FOREACH t IN ARRAY ARRAY['accounts','operations','transactions','transaction_versions','journal_entries','journal_lines'] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY',t);
        EXECUTE format('CREATE POLICY owned ON %I USING (EXISTS (SELECT 1 FROM ledgers WHERE id=ledger_id)) WITH CHECK (EXISTS (SELECT 1 FROM ledgers WHERE id=ledger_id))',t);
    END LOOP;
END $$;
