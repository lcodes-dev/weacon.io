//! Cutting pages and ranking their parts, with the real model, which the first run fetches.

use doc_search::{embed, parts, rank};

const PAGES: [(&str, &str); 4] = [
    (
        "Sign in",
        "Open the sign-in page and type your email.\n\n## The link\n\nWe mail you a link that signs you in. It works once, for fifteen minutes.\n\n```sh\n# not a heading\ncurl /login\n```\n",
    ),
    (
        "Billing",
        "## Plans\n\nPick a monthly or a yearly plan. You pay by card.\n\n## Cancel\n\nCancelling stops the next charge; you keep the plan until the period ends.\n",
    ),
    (
        "Teams",
        "## Invite\n\nAn owner invites people by email, and each takes a seat.\n\nThe `seat_count` column is checked in `app/Teams/Seats.php`.\n",
    ),
    (
        "Background jobs",
        "## Queues\n\nSlow work runs on a queue. A job that fails is tried three times.\n",
    ),
];

#[test]
fn a_page_is_cut_at_its_headings() {
    let cut = parts(PAGES[0].0, PAGES[0].1);
    assert_eq!(cut.len(), 2, "{cut:?}");
    assert_eq!(
        cut[0],
        "Sign in\nOpen the sign-in page and type your email.\n\n"
    );
    assert!(cut[1].starts_with("Sign in\n## The link\n") && cut[1].contains("# not a heading"));
    // A page that starts with a heading has no empty part before it, and an empty page has none.
    assert_eq!(parts(PAGES[1].0, PAGES[1].1).len(), 2);
    assert!(parts("Empty", "\n").is_empty());
}

/// The part each question finds first, among all the pages' parts.
fn found(questions: &[&str]) -> Vec<String> {
    let cut: Vec<String> = PAGES
        .iter()
        .flat_map(|(title, text)| parts(title, text))
        .collect();
    let embedded = embed(&cut).unwrap();
    let all: Vec<(&str, &[f32])> = cut
        .iter()
        .map(String::as_str)
        .zip(embedded.iter().map(Vec::as_slice))
        .collect();
    let questions: Vec<String> = questions
        .iter()
        .map(|question| question.to_string())
        .collect();
    let asked = embed(&questions).unwrap();
    let first = questions.iter().zip(&asked).map(|(question, asked)| {
        let order = rank(question, asked, &all);
        assert_eq!(order.len(), all.len());
        cut[order[0]]
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join(" / ")
    });
    first.collect()
}

#[test]
fn a_question_finds_the_part_about_it() {
    let first = found(&[
        "Customer still charged after they stopped their subscription",
        "Retry work that broke",
        "Log in without a password",
    ]);
    assert_eq!(
        first,
        [
            "Billing / ## Cancel",
            "Background jobs / ## Queues",
            "Sign in / ## The link"
        ]
    );
}

#[test]
fn a_name_written_exactly_comes_first() {
    // The first is worded like billing; the column's name says where to look.
    let first = found(&[
        "Customers pay for more than seat_count says",
        "What does Seats.php check?",
    ]);
    assert_eq!(first, ["Teams / ## Invite", "Teams / ## Invite"]);
}
