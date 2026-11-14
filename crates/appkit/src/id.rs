use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// An id for a request, a report or a row; it is unique, not secret, so never use it as a
/// token or code.
// Each `RandomState` gets new keys; the first on each thread come from the operating system.
#[must_use]
pub fn random_id() -> u128 {
    let half = || RandomState::new().build_hasher().finish();
    u128::from(half()) << 64 | u128::from(half())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_random_in_both_halves() {
        let id = random_id();
        let (high, low) = (id >> 64, id & u128::from(u64::MAX));

        assert_ne!(id, random_id());
        assert_ne!(high, 0);
        assert_ne!(high, low);
    }
}
