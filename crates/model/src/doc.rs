use std::collections::BTreeSet;
use std::fmt;

use weacon_protocol::{DocAnchors, DocId, DocPin, links_to_code};

use crate::{Revision, Title};

const BODY_MAX: usize = 256 * 1024;
const PINS_MAX: usize = 256;

/// A doc's title, Markdown body and pins, where every link to code has exactly one pin and
/// every pin a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocText {
    title: Title,
    body: String,
    anchors: Option<DocAnchors>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocTextError {
    TooLarge,
    /// The body links to an anchor without a pin.
    Unpinned,
    /// A pin or a commit that no link needs, or two pins for one anchor.
    Unlinked,
}

impl fmt::Display for DocTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "the doc is larger than the limit",
            Self::Unpinned => "the doc links to an anchor it has no pin for",
            Self::Unlinked => "the doc's pins don't match its links",
        })
    }
}

impl std::error::Error for DocTextError {}

impl DocText {
    /// # Errors
    /// When the body or its pins are over the limit, or the pins don't match the links.
    pub fn new(
        title: Title,
        body: String,
        anchors: Option<DocAnchors>,
    ) -> Result<Self, DocTextError> {
        let pins = anchors.as_ref().map_or(&[][..], |anchors| &anchors.pins);
        if body.len() > BODY_MAX || pins.len() > PINS_MAX {
            return Err(DocTextError::TooLarge);
        }
        let pinned: BTreeSet<&str> = pins.iter().map(DocPin::anchor).collect();
        let linked = links(&body);
        if !linked.is_subset(&pinned) {
            return Err(DocTextError::Unpinned);
        }
        // With every link pinned, as many links as pins leaves no pin spare or repeated.
        if linked.len() != pins.len() || (anchors.is_some() && pins.is_empty()) {
            return Err(DocTextError::Unlinked);
        }
        Ok(Self {
            title,
            body,
            anchors,
        })
    }

    #[must_use]
    pub const fn title(&self) -> &Title {
        &self.title
    }

    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    #[must_use]
    pub const fn anchors(&self) -> Option<&DocAnchors> {
        self.anchors.as_ref()
    }
}

/// The code anchors `body` may link to: each target after `](`, `<` or `]:`, up to a space
/// or the `)` or `>` that closes it, that [`may_link_to_code`]. It finds at least what Markdown links, so
/// a doubtful link is asked for a pin rather than missed.
fn links(body: &str) -> BTreeSet<&str> {
    [("](", ')'), ("<", '>'), ("]:", ' ')]
        .into_iter()
        .flat_map(|(opener, end)| body.split(opener).skip(1).map(move |rest| (rest, end)))
        .filter_map(|(rest, end)| destination(rest.trim_start(), end))
        .filter(|target| may_link_to_code(target))
        .collect()
}

/// The target at the start of `rest`, up to a space or the `end` that closes it. Inside
/// `(…)`, balanced parentheses belong to the target, as in Markdown.
fn destination(rest: &str, end: char) -> Option<&str> {
    let mut depth = 0_usize;
    for (index, char) in rest.char_indices() {
        match char {
            '(' => depth = depth.saturating_add(1),
            ')' if depth > 0 => depth -= 1,
            _ if char.is_whitespace() || char == end => return rest.get(..index),
            _ => {}
        }
    }
    Some(rest)
}

/// Whether Markdown could render `target` as a link to code: its kind is code in any case,
/// or what precedes its first `:` holds an escape or a character reference, which Markdown
/// decodes.
fn may_link_to_code(target: &str) -> bool {
    let scheme = target.split_once(':').map_or(target, |(scheme, _)| scheme);
    links_to_code(&target.to_ascii_lowercase()) || scheme.contains(['&', '\\'])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub id: DocId,
    pub text: DocText,
    pub version: Revision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviseError {
    /// The doc was revised since the revision the new text was written against.
    Stale { current: Revision },
    /// The doc is at the last revision there is.
    LastVersion,
}

impl fmt::Display for ReviseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stale { .. } => "the doc has changed since it was read",
            Self::LastVersion => "the doc can't be revised any more",
        })
    }
}

impl std::error::Error for ReviseError {}

impl Doc {
    /// A new doc, at its first revision.
    #[must_use]
    pub const fn new(id: DocId, text: DocText) -> Self {
        Self {
            id,
            text,
            version: Revision::FIRST,
        }
    }

    /// The doc with `text`, one revision on. `read_at` is the revision the text was written
    /// against: any other than the current one is refused.
    ///
    /// # Errors
    /// When the doc has changed since `read_at`, or no revision is left.
    pub fn revise(mut self, read_at: Revision, text: DocText) -> Result<Self, ReviseError> {
        if read_at != self.version {
            return Err(ReviseError::Stale {
                current: self.version,
            });
        }
        self.version = self.version.next().ok_or(ReviseError::LastVersion)?;
        self.text = text;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{CommitId, DocPin};

    use super::*;

    const FINGERPRINT: &str = "0123456789abcdef";

    fn title(text: &str) -> Title {
        Title::try_from(text.to_owned()).unwrap()
    }

    fn anchors(names: &[&str]) -> DocAnchors {
        let pins = names
            .iter()
            .map(|name| DocPin::try_from(format!("{name}#{FINGERPRINT}")).unwrap())
            .collect();
        DocAnchors {
            commit: CommitId::try_from("a".repeat(40)).unwrap(),
            pins,
        }
    }

    fn text(body: &str, pinned: &[&str]) -> Result<DocText, DocTextError> {
        let anchors = (!pinned.is_empty()).then(|| anchors(pinned));
        DocText::new(title("Framing"), body.to_owned(), anchors)
    }

    #[test]
    fn every_anchor_link_has_one_pin_and_every_pin_a_link() {
        let body = "See [encode](sym:a::encode \"Encode\"), the [flow](flow:a::b->c::d) and <sym:a::encode>\n\
                    [the area](area:crates/protocol/src/lib) again: [x](sym:a::encode).\n\
                    Plain [links](https://example.com/?a=1&b=2) and <b>tags</b> need no pin.";
        let all = [
            "sym:a::encode",
            "flow:a::b->c::d",
            "area:crates/protocol/src/lib",
        ];

        let made = text(body, &all).unwrap();
        assert_eq!(made.body(), body);
        assert_eq!(made.title(), &title("Framing"));
        assert_eq!(made.anchors(), Some(&anchors(&all)));
        assert!(text("No links at all.", &[]).unwrap().anchors().is_none());
        text("[a](sym:a(b)) [c](area:(c)d)", &["sym:a(b)", "area:(c)d"]).unwrap();
        for (body, pinned) in [
            ("[a](sym:a)", &[][..]),
            ("[a]( sym:a)", &[][..]),
            ("[a](\nsym:a)", &[][..]),
            ("[a][r]\n\n[r]: sym:a", &[][..]),
            ("[r]:sym:a", &[][..]),
            ("<SYM:a>", &[][..]),
            ("[a](Area:a)", &["area:a"][..]),
            ("[a](sym&#58;a)", &[][..]),
            ("[a](&#115;ym:a)", &[][..]),
            ("[a](sym&colon;a)", &[][..]),
            ("[a](sym\\:a)", &[][..]),
            ("[a](sym:a(b))", &["sym:a(b"][..]),
            ("[a](sym:a) <area:b>", &["sym:a"][..]),
            ("[a](sym:a)\n[b](flow:a->b)", &["sym:a"][..]),
        ] {
            assert_eq!(text(body, pinned), Err(DocTextError::Unpinned), "{body}");
        }
        for (body, pinned) in [
            ("No links.", &["sym:a"][..]),
            ("[a](sym:a)", &["sym:a", "sym:b"][..]),
            ("[a](sym:a)", &["sym:a", "sym:a"][..]),
        ] {
            assert_eq!(text(body, pinned), Err(DocTextError::Unlinked), "{body}");
        }
        let commit_only = Some(DocAnchors {
            pins: Vec::new(),
            ..anchors(&["sym:a"])
        });
        assert_eq!(
            DocText::new(title("Framing"), "No links.".to_owned(), commit_only),
            Err(DocTextError::Unlinked)
        );
    }

    #[test]
    fn a_body_and_its_pins_have_limits() {
        let longest = "a".repeat(BODY_MAX);
        text(&longest, &[]).unwrap();
        assert_eq!(
            text(&format!("{longest}a"), &[]),
            Err(DocTextError::TooLarge)
        );
        let names: Vec<String> = (0..=PINS_MAX).map(|n| format!("sym:a{n}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let body = |names: &[&str]| {
            names
                .iter()
                .map(|name| format!("[x]({name})"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let most = &names[..PINS_MAX];

        text(&body(most), most).unwrap();
        assert_eq!(text(&body(&names), &names), Err(DocTextError::TooLarge));
    }

    fn doc() -> Doc {
        let id = DocId::try_from("d-1".to_owned()).unwrap();
        Doc::new(id, text("First.", &[]).unwrap())
    }

    #[test]
    fn each_revision_replaces_the_text_and_counts_on() {
        let second = text("Second.", &[]).unwrap();
        let third = text("[a](sym:a)", &["sym:a"]).unwrap();

        assert_eq!(doc().version, Revision::FIRST);
        let revised = doc().revise(Revision::FIRST, second.clone()).unwrap();
        assert_eq!(
            (&revised.text, revised.version),
            (&second, Revision::new(2))
        );
        let revised = revised.revise(Revision::new(2), third.clone()).unwrap();
        assert_eq!((&revised.text, revised.version), (&third, Revision::new(3)));
    }

    #[test]
    fn a_revision_against_another_or_past_the_last_is_refused() {
        let next = || text("Next.", &[]).unwrap();
        let revised = doc().revise(Revision::FIRST, next()).unwrap();
        for read_at in [Revision::FIRST, Revision::new(3)] {
            assert_eq!(
                revised.clone().revise(read_at, next()),
                Err(ReviseError::Stale {
                    current: Revision::new(2)
                })
            );
        }
        let last = Doc {
            version: Revision::new(u64::MAX),
            ..doc()
        };
        assert_eq!(
            last.revise(Revision::new(u64::MAX), next()),
            Err(ReviseError::LastVersion)
        );
    }

    #[test]
    fn errors_say_what_went_wrong() {
        for (error, message) in [
            (
                DocTextError::TooLarge.to_string(),
                "the doc is larger than the limit",
            ),
            (
                DocTextError::Unpinned.to_string(),
                "the doc links to an anchor it has no pin for",
            ),
            (
                DocTextError::Unlinked.to_string(),
                "the doc's pins don't match its links",
            ),
            (
                ReviseError::Stale {
                    current: Revision::FIRST,
                }
                .to_string(),
                "the doc has changed since it was read",
            ),
            (
                ReviseError::LastVersion.to_string(),
                "the doc can't be revised any more",
            ),
        ] {
            assert_eq!(error, message);
        }
    }
}
