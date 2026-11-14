use std::fmt;

use weacon_observability::Fault;
use weacon_protocol::code;

const MAX_LENGTH: usize = 254;

/// Characters a mail header can't misread, besides ASCII letters and digits.
const LOCAL: &str = "!#$%&'*+/=?^_`{|}~.-";
const DOMAIN: &str = ".-";

/// An email address, trimmed and lowercased so that each address has one spelling.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Email(String);

impl Email {
    pub(crate) fn parse(text: &str) -> Result<Self, Fault> {
        let email = text.trim().to_lowercase();
        let made_of = |part: &str, extra: &str| {
            part.chars()
                .all(|letter| letter.is_ascii_alphanumeric() || extra.contains(letter))
        };
        let valid = email.len() <= MAX_LENGTH
            && email.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty()
                    && made_of(local, LOCAL)
                    && made_of(domain, DOMAIN)
                    && domain.contains('.')
                    && domain.split('.').all(|label| !label.is_empty())
            });
        if valid {
            Ok(Self(email))
        } else {
            Err(Fault::new(
                code!("email.invalid"),
                "That isn't an email address",
            ))
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// What follows the `@`.
    pub(crate) fn domain(&self) -> &str {
        self.0.split_once('@').map_or("", |(_, domain)| domain)
    }
}

// An address never reaches a log line, even through `{:?}`.
impl fmt::Debug for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Email(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_email_addresses_are_accepted() {
        let domain = "@example.com";
        let longest = format!("{}{domain}", "a".repeat(MAX_LENGTH - domain.len()));
        let long = format!("a{longest}");

        assert_eq!(
            Email::parse(" Ada@Example.COM ").unwrap().as_str(),
            "ada@example.com"
        );
        assert_eq!(Email::parse(&longest).unwrap().as_str(), longest);
        assert_eq!(
            Email::parse("o'brien+tag@mail.example.com")
                .unwrap()
                .as_str(),
            "o'brien+tag@mail.example.com"
        );
        assert_eq!(
            format!("{:?}", Email::parse("ada@example.com").unwrap()),
            "Email(..)"
        );
        for bad in [
            "",
            "ada",
            "@example.com",
            "ada@",
            "ada@example",
            "a da@example.com",
            "a@b@c.com",
            "ada@.com",
            "ada@example..com",
            "ada\u{1b}@example.com",
            "a,b@example.com",
            "<ada>@example.com",
            "ada;bo@example.com",
            "ada@exa_mple.com",
            &long,
        ] {
            assert_eq!(
                Email::parse(bad).unwrap_err().code().as_str(),
                "email.invalid",
                "{bad}"
            );
        }
    }
}
