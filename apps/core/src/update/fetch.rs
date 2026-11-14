//! Fetches the latest release's signed manifest and its builds over HTTPS. A request carries
//! nothing about this machine or its owner beyond what any HTTPS request does.

use std::io::Read;
use std::time::Duration;

use weacon_observability::Fault;
use weacon_protocol::{ReleaseManifest, code};

use super::verify;

pub(crate) const MANIFEST_URL: &str = "https://weacon.io/releases/stable/manifest.json";
pub(crate) const SIGNATURE_URL: &str = "https://weacon.io/releases/stable/manifest.json.minisig";

const MANIFEST_MAX: u64 = 64 * 1024;
const SIGNATURE_MAX: u64 = 4 * 1024;

/// A response's body, read as it arrives.
pub(crate) type Body = Box<dyn Read + Send>;

/// Opens the body at an `https://` address; tests pass a fake.
pub(crate) type Get<'a> = &'a dyn Fn(&str) -> Result<Body, Fault>;

/// Gets over HTTPS only, following redirects only to HTTPS.
pub(crate) fn https() -> impl Fn(&str) -> Result<Body, Fault> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .user_agent("weacon")
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_mins(10)))
        .build()
        .into();
    move |url: &str| {
        let response = agent
            .get(url)
            .call()
            .map_err(|error| fetch_failed().because(error))?;
        let reader = response
            .into_body()
            .into_with_config()
            .limit(u64::MAX)
            .reader();
        let body: Body = Box::new(reader);
        Ok(body)
    }
}

/// The latest release, once its signature shows one of `keys` signed it.
pub(crate) fn release(get: Get<'_>, keys: &[&str]) -> Result<ReleaseManifest, Fault> {
    let manifest = read_capped(get(MANIFEST_URL)?, MANIFEST_MAX)?;
    let signature = read_capped(get(SIGNATURE_URL)?, SIGNATURE_MAX)?;
    let signature = String::from_utf8(signature).map_err(|_| {
        Fault::new(
            code!("update.unsigned"),
            "The release isn't signed by a key this build trusts",
        )
    })?;
    verify::verified(&manifest, &signature, keys)
}

fn read_capped(body: Body, max: u64) -> Result<Vec<u8>, Fault> {
    let mut bytes = Vec::new();
    body.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| fetch_failed().because(error))?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > max) {
        return Err(Fault::new(
            code!("update.too_large"),
            "The release's manifest is too large",
        ));
    }
    Ok(bytes)
}

fn fetch_failed() -> Fault {
    Fault::new(
        code!("update.fetch_failed"),
        "The latest release couldn't be fetched",
    )
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};

    use super::*;
    use crate::update::fixtures::{MANIFEST, SIGNATURE, TEST_KEY};

    fn serving(
        manifest: &'static str,
        signature: &'static [u8],
    ) -> impl Fn(&str) -> Result<Body, Fault> {
        move |url: &str| {
            let body: Body = match url {
                MANIFEST_URL => Box::new(Cursor::new(manifest)),
                SIGNATURE_URL => Box::new(Cursor::new(signature)),
                _ => return Err(fetch_failed()),
            };
            Ok(body)
        }
    }

    #[test]
    fn the_latest_release_is_read_once_its_signature_checks_out() {
        let manifest = release(&serving(MANIFEST, SIGNATURE.as_bytes()), &[TEST_KEY]).unwrap();
        assert_eq!(manifest.version.to_string(), "1.2.3");
        let fault =
            release(&serving(MANIFEST, SIGNATURE.as_bytes()), &verify::TRUSTED).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.unsigned");
    }

    #[test]
    fn oversized_answers_are_refused() {
        let huge: &'static str = "x".repeat(65 * 1024).leak();
        let fault = release(&serving(huge, SIGNATURE.as_bytes()), &[TEST_KEY]).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.too_large");
        let long: &'static str = "x".repeat(5 * 1024).leak();
        let fault = release(&serving(MANIFEST, long.as_bytes()), &[TEST_KEY]).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.too_large");
        // Minisign ignores the padding, so only the size limit could refuse it.
        let padded: &'static str = format!("{SIGNATURE:<4096}").leak();
        release(&serving(MANIFEST, padded.as_bytes()), &[TEST_KEY]).unwrap();
        let exact: &'static str = "x".repeat(64 * 1024).leak();
        let fault = release(&serving(exact, SIGNATURE.as_bytes()), &[TEST_KEY]).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.unsigned");
    }

    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::TimedOut))
        }
    }

    #[test]
    fn a_failed_fetch_says_so() {
        let failing = |_: &str| -> Result<Body, Fault> { Ok(Box::new(Broken)) };
        let fault = release(&failing, &[TEST_KEY]).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.fetch_failed");
        let fault = release(&serving(MANIFEST, b"\xff\xfe"), &[TEST_KEY]).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.unsigned");
    }

    #[test]
    fn plain_http_is_refused() {
        let fault = https()("http://127.0.0.1:9/manifest.json").err().unwrap();
        assert_eq!(fault.code().as_str(), "update.fetch_failed");
    }
}
