-- OIDC: accounts are keyed by the identity provider's subject. A password-less account
-- (`password_hash = ''`) is one the provider owns; an older account is adopted by email.
ALTER TABLE accounts ADD COLUMN subject TEXT;
ALTER TABLE accounts ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX accounts_subject ON accounts (subject) WHERE subject IS NOT NULL;
