-- Run once as the schema owner after `sumi migrate`.
-- Create a separate LOGIN role named sumi_app and set its password out of band.
GRANT USAGE ON SCHEMA public TO sumi_app;
GRANT USAGE ON DOMAIN minor_amount TO sumi_app;
GRANT SELECT,INSERT ON users,ledgers,accounts,transactions,transaction_versions,journal_entries,journal_lines,operations TO sumi_app;
GRANT UPDATE (revision) ON ledgers TO sumi_app;
GRANT UPDATE (name,kind,currency,active,version) ON accounts TO sumi_app;
GRANT UPDATE (current_version,state) ON transactions TO sumi_app;
-- No UPDATE/DELETE/TRUNCATE grants on journal or history tables.
GRANT SELECT,INSERT ON preferences,categories,tags,sources,source_versions,observations,observation_versions,observation_history,source_positions,import_batches,batch_sources,batch_observations,evidence_links,transaction_identifiers,fx_rates,reconciliations TO sumi_app;
GRANT UPDATE (version,data) ON preferences TO sumi_app;
GRANT UPDATE (name,kind,parent_id,active,version) ON categories TO sumi_app;
GRANT UPDATE (name,active,version) ON tags TO sumi_app;
GRANT UPDATE (current_version_id) ON sources TO sumi_app;
GRANT UPDATE (kind,current_version,version,review_note,ignored_reason) ON observations TO sumi_app;
GRANT UPDATE (version,name,complete) ON import_batches TO sumi_app;
GRANT UPDATE (closed_revision,closed_operation_id) ON evidence_links TO sumi_app;
GRANT UPDATE (reporting_currency,timezone) ON ledgers TO sumi_app;
GRANT UPDATE (parent_id,external_identifiers) ON accounts TO sumi_app;
GRANT SELECT,INSERT ON authorization_grants TO sumi_app;
