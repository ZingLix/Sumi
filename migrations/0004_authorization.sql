ALTER TABLE users ADD COLUMN status text NOT NULL DEFAULT 'active' CHECK(status IN ('active','disabled'));
ALTER TABLE ledgers ADD UNIQUE(id,owner_user_id);
CREATE TABLE authorization_grants (
 ledger_id uuid NOT NULL,user_id uuid NOT NULL,client_id text NOT NULL,
 scopes text[] NOT NULL DEFAULT ARRAY['sumi:read','sumi:write'],
 status text NOT NULL DEFAULT 'active' CHECK(status IN ('active','revoked')),
 PRIMARY KEY(ledger_id,client_id),
 FOREIGN KEY(ledger_id,user_id) REFERENCES ledgers(id,owner_user_id)
);
ALTER TABLE authorization_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE authorization_grants FORCE ROW LEVEL SECURITY;
CREATE POLICY owned ON authorization_grants USING (EXISTS(SELECT 1 FROM ledgers WHERE id=ledger_id)) WITH CHECK(EXISTS(SELECT 1 FROM ledgers WHERE id=ledger_id));
