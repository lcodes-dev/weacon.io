use std::sync::RwLock;

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

type Transport = AsyncSmtpTransport<Tokio1Executor>;
type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Deserialize, Serialize, FromRow)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: Option<String>,
    #[serde(skip_serializing)]
    pub password: Option<String>,
    /// Such as `Weacon <weacon@example.com>`.
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

/// Prints mail to the log while it holds no SMTP connection.
#[derive(Default)]
pub struct Mailer(RwLock<Option<(Transport, Mailbox)>>);

impl Mailer {
    /// `None` goes back to the log.
    pub fn set(&self, smtp: Option<&Smtp>) -> Result<(), Error> {
        *self.0.write().unwrap() = smtp.map(connection).transpose()?;
        Ok(())
    }

    pub async fn send(&self, to: &str, subject: &str, text: &str) -> Result<(), Error> {
        let connection = self.0.read().unwrap().clone();
        let Some((transport, from)) = connection else {
            tracing::info!(
                "no SMTP, so this mail is only logged\nto: {to}\nsubject: {subject}\n\n{text}\n"
            );
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
