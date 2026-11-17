# Plan 03: HTML mail

**Status: done** (2026-11-17)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: every mail the server sends is an HTML mail with a plain-text fallback, laid out as a logo, then a card, then a footer.

## What a mail looks like

```
                ◯                    the logo

  ┌───────────────────────────────┐
  │ Your Weacon sign-in code      │  the subject, as a heading
  │                               │
  │ Enter this code to sign in.   │  the text, one paragraph per
  │ It works for 10 minutes.      │  blank-line-separated block
  │                               │
  │          817916               │  the code, large (only on code mails)
  └───────────────────────────────┘

  Sent by Weacon. If you didn't       the footer, the same on every mail
  ask for this mail, ignore it.
```

- **Logo.** The Weacon icon, 56px, centered. It travels inside each mail as an inline attachment, so it shows without the server having a public address. Mail clients don't show SVG, so `logo.png` is rendered from `logo.svg`; both live in `apps/server/assets/`.
- **Card.** White, rounded, with a thin border, 480px wide and centered on the reference's light background.
- **Colors.** The reference's light palette only. Mail clients handle dark mode unevenly, and a light mail reads fine in all of them.
- **Markup.** A table layout with inline styles, which is what mail clients render reliably. No web fonts, no links yet.

## How it is built

**One template.** A string in `mail.rs`, `TEMPLATE`, holds the whole layout with three placeholders: `{subject}`, `{body}` and `{footer}`. It is filled with `str::replace`, so `src/` holds only Rust. No template engine.

**One way to send.** `Mailer::send` takes a small struct in place of today's three arguments:

```rust
pub struct Mail<'a> {
    pub subject: &'a str,
    pub text: &'a str,          // paragraphs, separated by blank lines
    pub code: Option<&'a str>,  // shown large, after the text
}
```

From it `mail.rs` makes both parts of the mail:

| Part | Made from |
| --- | --- |
| Plain text | The paragraphs, the code on its own line, then the footer |
| HTML | The template, with each paragraph as a `<p>`, the code as a large centered line, and the logo attached inline |

Text is HTML-escaped before it goes into the template, by a five-line function: no dependency.

**Sending.** The two parts go out as one `multipart/alternative` message, which lettre builds, with the logo attached to the HTML part; a mail client shows the HTML and falls back to the text. No new dependency: lettre's `builder` feature already covers it.

**Without SMTP** nothing changes: the log gets the plain-text part only.

## The two mails

| Mail | Subject | Text | Code |
| --- | --- | --- | --- |
| Sign-in code | Your Weacon sign-in code | "Enter this code to sign in. It works for 10 minutes." | The 6 digits |
| Test mail | Weacon test mail | "This is a test mail from Weacon." / "Your mail settings work." | None |

## Server code

```
apps/server/src/mail.rs      Mail, the two parts, the layout, the escape function
apps/server/assets/logo.svg  new: the logo
apps/server/assets/logo.png  new: the logo as mail clients need it
apps/server/src/auth.rs      builds the sign-in mail as a Mail
apps/server/src/config.rs    builds the test mail as a Mail
```

**Tests.** No new automated test: no request can read a sent mail, and the existing tests keep covering that codes are mailed and that SMTP is used. The HTML is checked by eye, in the browser and in a real mail client.

## Steps

- [x] The layout, and `Mail` with its two parts, in `mail.rs`
- [x] The sign-in and test mails use it
- [x] Docs: the "Mail" section of `docs/architecture.md`
- [x] Checked in the browser: both mails sent to a local SMTP catcher and their HTML opened
- [x] Checked by hand: both mails in a real mail client, sent over SMTP

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| Dark colors | When you ask |
| Buttons and links in a mail | The first mail that needs one, such as an invitation; it also needs the server to know its public address |
| A template engine, or more than one layout | A mail the three placeholders can't express |
