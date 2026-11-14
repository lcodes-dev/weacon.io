use std::str::FromStr;

use crate::email::Email;

/// One `--sign-up`: who may make an account by signing in. Nobody may without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignUp {
    Anyone,
    /// The addresses at exactly this domain, not its subdomains.
    Domain(String),
}

impl FromStr for SignUp {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "anyone" {
            return Ok(Self::Anyone);
        }
        Email::parse(&format!("someone@{text}"))
            .map(|email| Self::Domain(email.domain().to_owned()))
            .map_err(|_| "neither `anyone` nor a domain such as example.com")
    }
}

/// Whether `email` may make an account under `rules`.
pub(crate) fn allows(rules: &[SignUp], email: &Email) -> bool {
    rules.iter().any(|rule| match rule {
        SignUp::Anyone => true,
        SignUp::Domain(domain) => email.domain() == domain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::email;

    fn rules(texts: &[&str]) -> Vec<SignUp> {
        texts.iter().map(|text| text.parse().unwrap()).collect()
    }

    fn allowed(texts: &[&str], address: &str) -> bool {
        allows(&rules(texts), &email(address))
    }

    #[test]
    fn a_rule_is_anyone_or_a_domain_in_lower_case() {
        assert_eq!(rules(&["anyone"]), [SignUp::Anyone]);
        assert_eq!(
            rules(&["Example.COM"]),
            [SignUp::Domain("example.com".to_owned())]
        );
        for wrong in [
            "",
            "@example.com",
            "ada@example.com",
            "localhost",
            "a..b",
            "Anyone",
        ] {
            assert!(wrong.parse::<SignUp>().is_err(), "{wrong}");
        }
    }

    #[test]
    fn only_the_rules_let_an_address_sign_up() {
        assert!(!allowed(&[], "ada@example.com"));
        assert!(allowed(&["anyone"], "ada@example.com"));
        assert!(allowed(&["other.org", "example.com"], "ada@example.com"));
        assert!(!allowed(&["example.com"], "ada@mail.example.com"));
        assert!(!allowed(&["example.com"], "ada@example.com.evil"));
        assert!(!allowed(&["mail.example.com"], "ada@example.com"));
    }
}
