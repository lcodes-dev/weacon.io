//! Random values from the operating system: tokens and the numbers codes are made from.

use weacon_observability::Fault;
use weacon_protocol::{code, to_hex};

fn random<const N: usize>() -> Result<[u8; N], Fault> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| random_failed())?;
    Ok(bytes)
}

/// A token nobody can guess: 32 random bytes, in hex.
pub(crate) fn random_token() -> Result<String, Fault> {
    Ok(to_hex(&random::<32>()?))
}

/// A random number, for a code's letters.
pub(crate) fn random_u64() -> Result<u64, Fault> {
    Ok(u64::from_le_bytes(random()?))
}

pub(crate) fn random_failed() -> Fault {
    Fault::new(
        code!("random.failed"),
        "The server couldn't get random bytes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_values_differ() {
        let token = random_token().unwrap();

        assert_eq!(token.len(), 64);
        assert_ne!(token, random_token().unwrap());
        assert_ne!(random_u64().unwrap(), random_u64().unwrap());
    }
}
