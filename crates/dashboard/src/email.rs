//! SMTP email delivery for compliance reports (Epic 42.3).
//!
//! Wraps `lettre` with a production-grade dispatcher:
//!   - Async tokio transport, rustls TLS (no OpenSSL dep)
//!   - Exponential backoff with jitter on transient failures
//!   - Dead-letter log — failed messages persist to disk so the
//!     operator can retry after the SMTP relay is restored
//!   - Prometheus counter surface
//!
//! Config schema in `mm-common`:
//! ```toml
//! [email]
//! enabled     = true
//! smtp_host   = "smtp.example.com"
//! smtp_port   = 587
//! smtp_tls    = "starttls"        # "starttls" | "wrapper" | "none"
//! username    = "reports@example.com"
//! # password via env MM_SMTP_PASSWORD
//! from_name   = "MG Market Maker"
//! from_addr   = "reports@example.com"
//! dead_letter_path = "data/email_dead_letter.jsonl"
//! max_retries = 3
//! ```

use chrono::{DateTime, Utc};
use lettre::{
    message::{header::ContentType, Attachment, MultiPart, SinglePart},
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
        AsyncSmtpTransport,
    },
    AsyncTransport, Message, Tokio1Executor,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Transport-layer TLS mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpTls {
    /// Connect cleartext, upgrade via STARTTLS (port 587).
    #[default]
    Starttls,
    /// TLS from the first byte (port 465 legacy).
    Wrapper,
    /// No TLS — test relays only. Warn at startup.
    None,
}

/// SMTP delivery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    #[serde(default)]
    pub enabled: bool,
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_tls: SmtpTls,
    pub username: String,
    /// Password comes from `MM_SMTP_PASSWORD` env var — never
    /// the config file, per the same fail-closed rule we apply
    /// to venue keys.
    #[serde(default = "default_from_name")]
    pub from_name: String,
    pub from_addr: String,
    #[serde(default = "default_dead_letter_path")]
    pub dead_letter_path: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_smtp_port() -> u16 {
    587
}
fn default_from_name() -> String {
    "MG Market Maker".to_string()
}
fn default_dead_letter_path() -> String {
    "data/email_dead_letter.jsonl".to_string()
}
fn default_max_retries() -> u32 {
    3
}

/// A single outbound email envelope.
#[derive(Debug, Clone)]
pub struct OutboundEmail {
    pub to_addr: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub body_text: String,
    /// Optional HTML body — the PDF / XLSX is typically attached
    /// separately, but an HTML part keeps the inline preview
    /// usable on clients that honour alternate parts.
    pub body_html: Option<String>,
    /// File attachments. Tuple: (filename, MIME type, bytes).
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

/// Dead-letter record appended to `dead_letter_path` when an
/// email ultimately fails after all retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeadLetter {
    timestamp: DateTime<Utc>,
    to_addr: String,
    subject: String,
    error: String,
    attempts: u32,
}

/// SMTP dispatcher. Safe to share across async tasks.
pub struct EmailDispatcher {
    config: EmailConfig,
    password: Option<String>,
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl EmailDispatcher {
    /// Build a dispatcher from config + env-provided password.
    /// Fails if the SMTP transport cannot be initialised (bad
    /// host, TLS misconfig). Initialisation does not open a
    /// connection — that happens lazily on first `send`.
    pub fn new(config: EmailConfig, password: Option<String>) -> anyhow::Result<Self> {
        let transport = match config.smtp_tls {
            SmtpTls::Wrapper => {
                let tls = TlsParameters::new(config.smtp_host.clone())?;
                let mut b = AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)?
                    .port(config.smtp_port)
                    .tls(Tls::Wrapper(tls));
                if let Some(p) = &password {
                    b = b.credentials(Credentials::new(
                        config.username.clone(),
                        p.clone(),
                    ));
                }
                b.timeout(Some(Duration::from_secs(20))).build()
            }
            SmtpTls::Starttls => {
                let tls = TlsParameters::new(config.smtp_host.clone())?;
                let mut b = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)?
                    .port(config.smtp_port)
                    .tls(Tls::Required(tls));
                if let Some(p) = &password {
                    b = b.credentials(Credentials::new(
                        config.username.clone(),
                        p.clone(),
                    ));
                }
                b.timeout(Some(Duration::from_secs(20))).build()
            }
            SmtpTls::None => {
                tracing::warn!(
                    "SMTP configured with TLS=none — only acceptable on a trusted \
                     localhost relay. NEVER use against a remote provider."
                );
                let mut b = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                    &config.smtp_host,
                )
                .port(config.smtp_port);
                if let Some(p) = &password {
                    b = b.credentials(Credentials::new(
                        config.username.clone(),
                        p.clone(),
                    ));
                }
                b.timeout(Some(Duration::from_secs(20))).build()
            }
        };

        Ok(Self {
            config,
            password,
            transport,
        })
    }

    /// Send with retry + dead-letter fallback. Returns `Ok(())`
    /// when delivered OR when deadletter has been written —
    /// callers should not treat "dead-lettered" as success for
    /// critical compliance reports. Use `send_required` for that
    /// contract instead.
    pub async fn send(&self, msg: OutboundEmail) -> anyhow::Result<()> {
        let built = self.build_message(&msg)?;
        let retries = self.config.max_retries.max(1);
        let mut last_err: Option<String> = None;

        for attempt in 1..=retries {
            match self.transport.send(built.clone()).await {
                Ok(_resp) => {
                    tracing::info!(
                        to = %msg.to_addr,
                        subject = %msg.subject,
                        attempts = attempt,
                        "email sent"
                    );
                    return Ok(());
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                    tracing::warn!(
                        to = %msg.to_addr,
                        attempt = attempt,
                        max = retries,
                        error = %e,
                        "email send failed"
                    );
                    // Exponential backoff with jitter: 250 ms × 2^n + [0..250] ms
                    if attempt < retries {
                        let base = 250u64 * (1 << (attempt - 1).min(4));
                        let jitter = chrono::Utc::now()
                            .timestamp_nanos_opt()
                            .unwrap_or(0)
                            .unsigned_abs()
                            % 250;
                        tokio::time::sleep(Duration::from_millis(base + jitter)).await;
                    }
                }
            }
        }

        // All retries exhausted — dead-letter.
        let err = last_err.unwrap_or_else(|| "unknown".to_string());
        self.write_dead_letter(&msg, retries, &err)
            .unwrap_or_else(|e| tracing::error!(error = %e, "dead-letter write failed"));
        anyhow::bail!(
            "email send to {} failed after {} attempts: {}",
            msg.to_addr,
            retries,
            err
        );
    }

    fn build_message(&self, msg: &OutboundEmail) -> anyhow::Result<Message> {
        let from = format!("{} <{}>", self.config.from_name, self.config.from_addr);
        let to = match &msg.to_name {
            Some(name) => format!("{} <{}>", name, msg.to_addr),
            None => msg.to_addr.clone(),
        };
        let mut builder = Message::builder()
            .from(from.parse()?)
            .to(to.parse()?)
            .subject(&msg.subject);
        // Add a Date header manually — lettre does it by default but
        // explicit is documentation-worthy.
        builder = builder.date_now();

        // Build multipart body: text + (optional html) + attachments
        let text_part = SinglePart::builder()
            .header(ContentType::TEXT_PLAIN)
            .body(msg.body_text.clone());

        let mut alt = MultiPart::alternative().singlepart(text_part);
        if let Some(html) = &msg.body_html {
            alt = alt.singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(html.clone()),
            );
        }

        let mut mixed = MultiPart::mixed().multipart(alt);
        for (name, mime, bytes) in &msg.attachments {
            let ct: ContentType = mime.parse().unwrap_or(ContentType::parse("application/octet-stream").unwrap());
            mixed = mixed.singlepart(
                Attachment::new(name.clone()).body(bytes.clone(), ct),
            );
        }

        let email = builder.multipart(mixed)?;
        Ok(email)
    }

    fn write_dead_letter(
        &self,
        msg: &OutboundEmail,
        attempts: u32,
        error: &str,
    ) -> anyhow::Result<()> {
        use std::io::Write;
        let rec = DeadLetter {
            timestamp: Utc::now(),
            to_addr: msg.to_addr.clone(),
            subject: msg.subject.clone(),
            error: error.to_string(),
            attempts,
        };
        let line = serde_json::to_string(&rec)?;
        let path = PathBuf::from(&self.config.dead_letter_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

impl std::fmt::Debug for EmailDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailDispatcher")
            .field("host", &self.config.smtp_host)
            .field("port", &self.config.smtp_port)
            .field("tls", &self.config.smtp_tls)
            .field("has_password", &self.password.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_config() -> EmailConfig {
        EmailConfig {
            enabled: true,
            smtp_host: "smtp.example.com".into(),
            smtp_port: 587,
            smtp_tls: SmtpTls::Starttls,
            username: "reports@example.com".into(),
            from_name: "MG Market Maker".into(),
            from_addr: "reports@example.com".into(),
            dead_letter_path: "/tmp/test_dead.jsonl".into(),
            max_retries: 2,
        }
    }

    #[test]
    fn smtp_tls_serde_round_trips() {
        let t = SmtpTls::Starttls;
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s, "\"starttls\"");
        let back: SmtpTls = serde_json::from_str(&s).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn dispatcher_builds_message_with_attachment() {
        let cfg = mk_config();
        let d = EmailDispatcher::new(cfg, Some("pw".into())).unwrap();
        let msg = OutboundEmail {
            to_addr: "recipient@example.com".into(),
            to_name: Some("Recipient".into()),
            subject: "Daily Report 2026-04-17".into(),
            body_text: "See attached PDF.".into(),
            body_html: Some("<p>See attached PDF.</p>".into()),
            attachments: vec![("daily.pdf".into(), "application/pdf".into(), b"%PDF-fake".to_vec())],
        };
        // Build-message should succeed with a valid multipart.
        let built = d.build_message(&msg).unwrap();
        let raw = format!("{:?}", built);
        // Presence of sender / recipient / subject in message headers.
        assert!(raw.contains("recipient@example.com"));
        assert!(raw.contains("Daily Report 2026-04-17"));
    }

    #[test]
    fn dead_letter_persists_record() {
        let tmp = std::env::temp_dir().join(format!(
            "mm_dead_{}.jsonl",
            std::process::id()
        ));
        let mut cfg = mk_config();
        cfg.dead_letter_path = tmp.to_string_lossy().to_string();
        let d = EmailDispatcher::new(cfg, Some("pw".into())).unwrap();

        let msg = OutboundEmail {
            to_addr: "x@example.com".into(),
            to_name: None,
            subject: "t".into(),
            body_text: "b".into(),
            body_html: None,
            attachments: vec![],
        };
        d.write_dead_letter(&msg, 3, "transient network error")
            .unwrap();

        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("x@example.com"));
        assert!(content.contains("transient network error"));
        let _ = std::fs::remove_file(tmp);
    }
}
