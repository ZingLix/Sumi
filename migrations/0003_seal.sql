-- Preserve journal sealing even when a physical backup is restored into a
-- cluster whose transaction counter may overlap the original cluster.
ALTER TABLE journal_entries ADD COLUMN created_tx_at timestamptz NOT NULL DEFAULT transaction_timestamp();
CREATE OR REPLACE FUNCTION protect_sealed_entry() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM journal_entries WHERE ledger_id=NEW.ledger_id AND id=NEW.entry_id
    AND created_xid=pg_current_xact_id() AND created_tx_at=transaction_timestamp()) THEN
   RAISE EXCEPTION 'Cannot append lines to a committed journal entry' USING ERRCODE='23514';
 END IF; RETURN NEW;
END; $$;
