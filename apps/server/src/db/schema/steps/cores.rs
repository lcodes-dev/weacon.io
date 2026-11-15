/// What each paired machine's core said it is, and which of the server's offers it offered
/// back, when it last connected.
pub(crate) const CORE_BUILDS: &str = "
ALTER TABLE machines ADD COLUMN core_build TEXT;
ALTER TABLE machines ADD COLUMN core_offers TEXT;
";
