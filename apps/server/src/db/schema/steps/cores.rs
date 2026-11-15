/// What each paired machine's core said it is, and what it offered, when it last connected.
pub(crate) const CORE_BUILDS: &str = "
ALTER TABLE machines ADD COLUMN core_name TEXT;
ALTER TABLE machines ADD COLUMN core_version TEXT;
ALTER TABLE machines ADD COLUMN core_commit TEXT;
ALTER TABLE machines ADD COLUMN core_offers TEXT;
";
