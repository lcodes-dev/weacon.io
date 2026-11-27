use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};

use super::db;
use crate::AppState;

type Transport = AsyncSmtpTransport<Tokio1Executor>;
type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Deserialize, Serialize)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: Option<String>,
    #[serde(skip_serializing)]
    pub password: Option<String>,
    pub from: String,
}

#[derive(Clone, Copy, Deserialize, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum Security {
    Tls,
    Starttls,
    None,
}

const LOGO: &[u8] = include_bytes!("../../assets/logo.png");
const FOOTER: &str = "Sent by Weacon. If you didn't ask for this mail, ignore it.";

pub struct Mail<'a> {
    pub subject: &'a str,
    pub text: &'a str,
    pub code: Option<&'a str>,
    pub button: Option<(&'a str, &'a str)>,
}

impl Mail<'_> {
    fn text(&self) -> String {
        let mut text = self.text.to_owned();
        if let Some(code) = self.code {
            text += &format!("\n\n{code}");
        }
        if let Some((label, address)) = self.button {
            text += &format!("\n\n{label}: {address}");
        }
        text + "\n\n" + FOOTER
    }

    fn html(&self) -> String {
        let mut body = String::new();
        for paragraph in self.text.split("\n\n") {
            body += &format!(r#"<p style="margin:0 0 12px">{}</p>"#, escape(paragraph));
        }
        if let Some(code) = self.code {
            body += &format!(
                r#"<p style="margin:20px 0 8px;font:600 30px/1 ui-monospace,Menlo,Consolas,monospace;letter-spacing:6px;text-align:center">{}</p>"#,
                escape(code)
            );
        }
        if let Some((label, address)) = self.button {
            body += &format!(
                r#"<p style="margin:20px 0 8px"><a href="{}" style="display:inline-block;padding:9px 16px;border-radius:8px;background:#1f1d1a;color:#ffffff;font-weight:600;text-decoration:none">{}</a></p>"#,
                escape(address),
                escape(label)
            );
        }
        TEMPLATE
            .replace("{subject}", &escape(self.subject))
            .replace("{body}", &body)
            .replace("{footer}", FOOTER)
    }
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{subject}</title>
  </head>
  <body style="margin:0;padding:0;background:#fbfaf7">
    <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:#fbfaf7">
      <tr>
        <td align="center" style="padding:32px 16px">
          <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:480px;font:15px/1.5 -apple-system,'Segoe UI',Helvetica,Arial,sans-serif;color:#1f1d1a">
            <tr>
              <td align="center" style="padding-bottom:24px">
                <img src="cid:logo" width="56" height="56" alt="Weacon" style="display:block;border:0">
              </td>
            </tr>
            <tr>
              <td style="padding:28px;background:#ffffff;border:1px solid #e7e3dc;border-radius:12px">
                <h1 style="margin:0 0 16px;font-size:19px;line-height:1.3;font-weight:600">{subject}</h1>
                {body}
              </td>
            </tr>
            <tr>
              <td align="center" style="padding:20px 28px 0;font-size:13px;color:#6b665e">{footer}</td>
            </tr>
          </table>
        </td>
      </tr>
    </table>
  </body>
</html>"##;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn is_address(email: &str) -> bool {
    email.parse::<Address>().is_ok()
}

pub fn check(smtp: &Smtp) -> Result<(), Error> {
    connection(smtp).map(drop)
}

pub async fn send(state: &AppState, to: &str, mail: &Mail<'_>) -> Result<(), Error> {
    let (subject, text) = (mail.subject, mail.text());
    let Some(smtp) = db::smtp(&state.pool, &state.server_key).await? else {
        tracing::info!("no SMTP, so this mail is only logged\nto: {to}\nsubject: {subject}\n\n{text}\n");
        return Ok(());
    };
    let (transport, from) = connection(&smtp)?;
    let logo = Attachment::new_inline("logo".into()).body(LOGO.to_vec(), ContentType::parse("image/png")?);
    let message =
        Message::builder().from(from).to(Mailbox::new(None, to.parse()?)).subject(subject).multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::plain(text))
                .multipart(MultiPart::related().singlepart(SinglePart::html(mail.html())).singlepart(logo)),
        )?;
    transport.send(message).await?;
    Ok(())
}

fn connection(smtp: &Smtp) -> Result<(Transport, Mailbox), Error> {
    let builder = match smtp.security {
        Security::Tls => Transport::relay(&smtp.host)?,
        Security::Starttls => Transport::starttls_relay(&smtp.host)?,
        Security::None => Transport::builder_dangerous(&smtp.host),
    };
    let builder = match (&smtp.username, &smtp.password) {
        (Some(username), Some(password)) => {
            builder.credentials(Credentials::new(username.clone(), password.clone()))
        }
        _ => builder,
    };
    Ok((builder.port(smtp.port).build(), smtp.from.parse()?))
}
