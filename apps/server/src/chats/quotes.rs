//! What a prompt's agent is given of its chat: what the others said since its owner's last
//! prompt there, as quotes.

use std::collections::HashMap;

use weacon_observability::Fault;
use weacon_protocol::{ChatId, MemberId, Quote, QuotedAgent};

use crate::db::{Said, Workspace};

/// The most bytes the quotes on one prompt take as JSON: 16 KiB.
pub(crate) const QUOTED_MAX: usize = 16 << 10;

/// What `chat`'s people and its other members' agents said after its event `after` and up to
/// `upto`, for `member`'s agent, the oldest first: as much of the newest as fits
/// [`QUOTED_MAX`].
pub(crate) async fn between(
    workspace: &Workspace,
    chat: &ChatId,
    member: &MemberId,
    stretch: (u64, u64),
) -> Result<Vec<Quote>, Fault> {
    let names = crate::workspaces::names(&workspace.members().await?);
    let mut read = 0_usize;
    let enough = |one: &Said| {
        read = read.saturating_add(one.text.len());
        read >= QUOTED_MAX
    };
    let said = workspace.said_between(chat, member, stretch, enough);
    let said = said.await?;
    let quotes = said.into_iter().map(|one| quote(one, &names));
    Ok(fit(quotes.collect(), QUOTED_MAX))
}

fn quote(said: Said, names: &HashMap<MemberId, String>) -> Quote {
    let name = names.get(&said.member);
    Quote {
        name: name.map_or_else(|| "A member".to_owned(), Clone::clone),
        agent: said
            .agent
            .map(|(kind, machine)| QuotedAgent { kind, machine }),
        text: said.text,
    }
}

// `newest_first` as the oldest-first quotes that take at most `room` bytes as JSON: the newest
// whole, and the start of the first one that doesn't fit.
fn fit(newest_first: Vec<Quote>, mut room: usize) -> Vec<Quote> {
    let mut kept = Vec::new();
    for mut quote in newest_first {
        let whole = size(&quote) <= room;
        while !quote.text.is_empty() && size(&quote) > room {
            // A byte takes six as JSON at most, so this never cuts more than it must.
            let over = size(&quote).saturating_sub(room).div_ceil(6);
            let mut end = quote.text.len().saturating_sub(over);
            while !quote.text.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            quote.text.truncate(end);
        }
        if quote.text.is_empty() {
            break;
        }
        room = room.saturating_sub(size(&quote));
        kept.push(quote);
        if !whole {
            break;
        }
    }
    kept.reverse();
    kept
}

// The bytes `quote` takes in a command's JSON, with the comma after it.
fn size(quote: &Quote) -> usize {
    serde_json::to_string(quote).map_or(usize::MAX, |json| json.len().saturating_add(1))
}

#[cfg(test)]
mod tests {
    use weacon_protocol::AgentKind;

    use super::*;

    fn quote(name: &str, text: &str) -> Quote {
        Quote {
            name: name.to_owned(),
            agent: None,
            text: text.to_owned(),
        }
    }

    fn total(quotes: &[Quote]) -> usize {
        quotes.iter().map(size).sum()
    }

    #[test]
    fn the_quotes_on_a_prompt_take_sixteen_kibibytes_at_most_and_the_newest_are_kept() {
        assert_eq!(QUOTED_MAX, 16_384);
        let newest_first = vec![
            quote("grace", &"c".repeat(6_000)),
            quote("ada", &"b".repeat(6_000)),
            quote("grace", &"a".repeat(6_000)),
            quote("ada", "never read"),
        ];

        let kept = fit(newest_first, QUOTED_MAX);

        let starts: Vec<_> = kept.iter().map(|quote| quote.text.get(..1)).collect();
        assert_eq!(starts, [Some("a"), Some("b"), Some("c")]);
        let lengths: Vec<_> = kept.iter().map(|quote| quote.text.len()).collect();
        assert_eq!(lengths, [4_305, 6_000, 6_000]);
        assert_eq!(total(&kept), QUOTED_MAX);
    }

    #[test]
    fn a_quote_is_cut_between_characters_and_counted_as_it_is_sent() {
        // Each takes six bytes as JSON, and each あ three.
        let escaped = quote("ada", &"\u{1}".repeat(40));
        let wide = quote("ada", &"あ".repeat(40));
        let room = size(&quote("ada", "")) + 62;

        let cut = fit(vec![escaped], room);
        let whole = fit(vec![wide.clone()], room);
        let none = fit(vec![wide.clone()], size(&quote("ada", "")) + 2);

        assert_eq!(cut[0].text.len(), 10);
        assert_eq!(whole[0].text, "あ".repeat(20));
        assert!(total(&cut) <= room && total(&whole) <= room);
        assert!(none.is_empty());
        assert_eq!(fit(vec![wide.clone()], usize::MAX), [wide]);
    }

    #[test]
    fn a_quote_names_its_writer_and_the_agent_that_wrote_it() {
        let names = HashMap::from([("m1".parse().unwrap(), "ada".to_owned())]);
        let said = |member: &str, agent| Said {
            member: member.parse().unwrap(),
            agent,
            text: "Done.".to_owned(),
        };
        let laptop = || (AgentKind::Codex, "laptop".parse().unwrap());

        let by_agent = super::quote(said("m1", Some(laptop())), &names);
        let by_stranger = super::quote(said("m9", None), &names);

        let agent = by_agent.agent.unwrap();
        assert_eq!(
            (by_agent.name.as_str(), agent.kind),
            ("ada", AgentKind::Codex)
        );
        assert_eq!(agent.machine.as_str(), "laptop");
        assert_eq!(
            (by_stranger.name.as_str(), by_stranger.agent),
            ("A member", None)
        );
    }
}
