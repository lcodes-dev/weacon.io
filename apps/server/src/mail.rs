use std::sync::RwLock;

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

type Transport = AsyncSmtpTransport<Tokio1Executor>;
type Error = Box<dyn std::error::Error + Send + Sync>;

/// The owner's SMTP settings, as stored and as the API carries them.
#[derive(Clone, Deserialize, Serialize, FromRow)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    /// `tls`, `starttls` or `none`.
    pub security: String,
    pub username: Option<String>,
    #[serde(skip_serializing)]
    pub password: Option<String>,
    /// The sender, such as `Weacon <weacon@example.com>`.
    pub from: String,
}

/// Sends mail over the owner's SMTP server, or prints it to the log while there is none.
#[derive(Default)]
pub struct Mailer(RwLock<Option<(Transport, Mailbox)>>);

impl Mailer {
    /// `None` goes back to the log.
    pub fn set(&self, smtp: Option<&Smtp>) -> Result<(), Error> {
        *self.0.write().unwrap() = smtp.map(connection).transpose()?;
        Ok(())
    }

    #[cfg(test)]
    pub fn uses_smtp(&self) -> bool {
        self.0.read().unwrap().is_some()
    }

    pub async fn send(&self, to: &str, subject: &str, text: &str) -> Result<(), Error> {
        let connection = self.0.read().unwrap().clone();
        let Some((transport, from)) = connection else {
            println!("mail to {to}\nsubject: {subject}\n\n{text}\n");
            return Ok(());
        };
        let message = Message::builder()
            .from(from)
            .to(to.parse()?)
            .subject(subject)
            .body(text.to_owned())?;
        transport.send(message).await?;
        Ok(())
    }
}

fn connection(smtp: &Smtp) -> Result<(Transport, Mailbox), Error> {
    let builder = match smtp.security.as_str() {
        "tls" => Transport::relay(&smtp.host)?,
        "starttls" => Transport::starttls_relay(&smtp.host)?,
        "none" => Transport::builder_dangerous(&smtp.host),
        _ => return Err("security must be tls, starttls or none".into()),
    };
    let builder = match (&smtp.username, &smtp.password) {
        (Some(username), Some(password)) => {
            builder.credentials(Credentials::new(username.clone(), password.clone()))
        }
        _ => builder,
    };
    Ok((builder.port(smtp.port).build(), smtp.from.parse()?))
}
