//! Signed with a key made for these tests, whose secret half was then thrown away.

pub(crate) const TEST_KEY: &str = "RWTGzWquLR59N1pXDkox+4RH+bJszZVLIB9t3Dl1O+2wU5eSZwF3H/2u";
pub(crate) const MANIFEST: &str = concat!(
    r#"{"version":"1.2.3","commit":"0123456789abcdef0123456789abcdef01234567","#,
    r#""published_at":1794614400,"protocol":1,"builds":[{"#,
    r#""target":"x86_64-unknown-linux-musl","#,
    r#""url":"https://example.com/weacon-x86_64-unknown-linux-musl","size":7148880,"#,
    r#""sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}]}"#,
    "\n"
);
pub(crate) const SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUTGzWquLR59N9nG/L2FVVYwAZk/0JaJuPmJWHELg+xq0LQ5v0mzp2GS8mQdv5IMdiIwjvBE2AH700UwTtkt+wBfz3IX9fme2gY=
trusted comment: test manifest
BJKm/Twbm6UE5SrGR40+jVd8EZZraJi5+z8RHhiwSsuvp+6yA5SLFoH0nGs0iIeXHJ6pjDAnJUhn4VIqbNX2Cg==
";
pub(crate) const LEGACY: &str = "untrusted comment: signature from minisign secret key
RWTGzWquLR59NySp5wMtiI0cCXW/CvvSHBMMZKeNcmAeM6FAcwNrztq5g4yF8Wq7tflpGSDKN9o0uCqMRkj4MZcCDTmhKEc8ZQg=
trusted comment: legacy
kM5rX8+bLA53QVhWe0U2A00AdVKO5EEIS4lY/tmrhGDBqTfMdieCJ/pB0AZBGtzRTYCI6fM0NyDv6rrGqg81Dw==
";
pub(crate) const OTHER: &str = "{\"version\":\"1.2\"}\n";
pub(crate) const OTHER_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUTGzWquLR59N5g8E6w+71VtUDPj6PmSr05tM4ozgON6dRE7LQFawm58y96JnRrDrb4gUdCK7uKrW3/4hboFk6CgsuN48+gDJQE=
trusted comment: not a manifest
34luRc6UU04v1sKriVrzzEIxAoyqMofwPqhAEWIU8MhnY+I0iepcOXV45b9eihUl6556Mn16d91VUUr2Z1iOBQ==
";
