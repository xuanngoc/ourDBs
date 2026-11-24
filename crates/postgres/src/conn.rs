//! Connection settings, TLS, opening sessions, cancel, and error text.

use std::time::Duration;

use anyhow::anyhow;
use native_tls::{Certificate, TlsConnector};
use ourdb_core::{ConnConfig, TlsMode};
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::{
    CancelToken, Client, Config, NoTls,
    config::SslMode,
    error::{DbError, ErrorPosition},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// TLS setup, kept for new sessions and for cancel requests.
#[derive(Clone)]
enum Tls {
    Off,
    Native(MakeTlsConnector),
}

/// Everything needed to open a new session to the server.
#[derive(Clone)]
pub struct Opts {
    config: Config,
    tls: Tls,
    read_only: bool,
}

impl Opts {
    pub fn new(cfg: &ConnConfig) -> anyhow::Result<Opts> {
        Ok(Opts {
            config: build_config(cfg)?,
            tls: build_tls(cfg)?,
            read_only: cfg.read_only,
        })
    }

    /// The database name sessions connect to.
    pub fn database(&self) -> Option<String> {
        self.config.get_dbname().map(str::to_string)
    }

    /// Open a new session. On a read-only connection the session is read-only too.
    pub async fn open(&self) -> anyhow::Result<Client> {
        let client = tokio::time::timeout(CONNECT_TIMEOUT, self.connect())
            .await
            .map_err(|_| anyhow!("Connection timed out after 10 s."))??;
        if self.read_only {
            client
                .batch_execute("SET default_transaction_read_only = on")
                .await
                .map_err(|e| anyhow!(error_text(&e)))?;
        }
        Ok(client)
    }

    async fn connect(&self) -> anyhow::Result<Client> {
        // The connection object does the socket I/O. It runs as its own task
        // and stops when the client is dropped or the socket closes.
        let client = match &self.tls {
            Tls::Off => {
                let (client, conn) = self
                    .config
                    .connect(NoTls)
                    .await
                    .map_err(|e| anyhow!(error_text(&e)))?;
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                client
            }
            Tls::Native(tls) => {
                let (client, conn) = self
                    .config
                    .connect(tls.clone())
                    .await
                    .map_err(|e| anyhow!(error_text(&e)))?;
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                client
            }
        };
        Ok(client)
    }

    /// Ask the server to stop the query that runs on the session of `token`.
    pub async fn cancel(&self, token: &CancelToken) {
        let _ = match &self.tls {
            Tls::Off => token.cancel_query(NoTls).await,
            Tls::Native(tls) => token.cancel_query(tls.clone()).await,
        };
    }
}

fn build_config(cfg: &ConnConfig) -> anyhow::Result<Config> {
    let mut c = if cfg.has_url() {
        let url = without_ssl_options(cfg.url.trim());
        let mut c: Config = url
            .parse()
            .map_err(|e| anyhow!("Invalid PostgreSQL URL: {}", error_text(&e)))?;
        if c.get_password().is_none() && !cfg.password.is_empty() {
            c.password(cfg.password.as_str());
        }
        // The database field wins over the URL path. The database picker uses this.
        if !cfg.database.trim().is_empty() {
            c.dbname(cfg.database.trim());
        }
        c
    } else {
        let mut c = Config::new();
        c.host(cfg.host_or_default()).port(cfg.port_or_default());
        if !cfg.user.trim().is_empty() {
            c.user(cfg.user.trim());
        }
        if !cfg.password.is_empty() {
            c.password(cfg.password.as_str());
        }
        if !cfg.database.trim().is_empty() {
            c.dbname(cfg.database.trim());
        }
        c
    };
    // libpq uses the login name when no user is given. Do the same.
    if c.get_user().is_none()
        && let Ok(user) = std::env::var("USER")
    {
        c.user(user);
    }
    if c.get_dbname().is_none() {
        c.dbname("postgres");
    }
    if c.get_application_name().is_none() {
        c.application_name("OurDB");
    }
    c.connect_timeout(CONNECT_TIMEOUT);
    c.ssl_mode(match cfg.tls {
        TlsMode::Off => SslMode::Disable,
        TlsMode::Prefer => SslMode::Prefer,
        TlsMode::Require | TlsMode::Verify => SslMode::Require,
    });
    Ok(c)
}

/// Remove `ssl*` options from a URL. The TLS fields of the connection decide
/// TLS, and tokio-postgres rejects libpq options such as `sslrootcert` or
/// `sslmode=verify-full`.
fn without_ssl_options(url: &str) -> String {
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return url.to_string();
    }
    // Look for '?' after the user info, because a password can contain '?'.
    let from = url.rfind('@').unwrap_or(0);
    let Some(q) = url[from..].find('?').map(|i| from + i) else {
        return url.to_string();
    };
    let kept: Vec<&str> = url[q + 1..]
        .split('&')
        .filter(|opt| {
            let key = opt.split('=').next().unwrap_or("");
            !key.is_empty() && (!key.starts_with("ssl") || key == "sslnegotiation")
        })
        .collect();
    if kept.is_empty() {
        url[..q].to_string()
    } else {
        format!("{}?{}", &url[..q], kept.join("&"))
    }
}

fn build_tls(cfg: &ConnConfig) -> anyhow::Result<Tls> {
    let mut b = TlsConnector::builder();
    match cfg.tls {
        TlsMode::Off => return Ok(Tls::Off),
        TlsMode::Prefer | TlsMode::Require => {
            b.danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }
        TlsMode::Verify => {
            let path = cfg.ca_file.trim();
            if !path.is_empty() {
                let pem = std::fs::read(path)
                    .map_err(|e| anyhow!("Cannot read the CA file {path}: {e}"))?;
                let certs = Certificate::stack_from_pem(&pem)
                    .map_err(|e| anyhow!("The CA file {path} is not a valid PEM file: {e}"))?;
                if certs.is_empty() {
                    anyhow::bail!("The CA file {path} has no certificates.");
                }
                for cert in certs {
                    b.add_root_certificate(cert);
                }
            }
            // Through an SSH tunnel the host is 127.0.0.1, so the name in the
            // certificate cannot match. Still check the chain.
            if cfg.via_tunnel {
                b.danger_accept_invalid_hostnames(true);
            }
        }
    }
    let connector = b.build().map_err(|e| anyhow!("Cannot set up TLS: {e}"))?;
    Ok(Tls::Native(MakeTlsConnector::new(connector)))
}

// ---------------------------------------------------------------------------
// Error text
// ---------------------------------------------------------------------------

/// Readable text for an error: the server message with DETAIL, HINT and the
/// position, or the error and its causes for other errors.
pub fn error_text(e: &tokio_postgres::Error) -> String {
    error_text_with(e, |p| Some(format!("Position: {p}")))
}

/// Like [`error_text`], but `position` turns the position from the server
/// (a character index from 1) into text, or drops it.
pub fn error_text_with(
    e: &tokio_postgres::Error,
    position: impl FnOnce(u32) -> Option<String>,
) -> String {
    match e.as_db_error() {
        Some(db) => {
            let pos = match db.position() {
                Some(ErrorPosition::Original(p)) => position(*p),
                _ => None,
            };
            db_text(db, pos)
        }
        None => other_text(e),
    }
}

fn db_text(db: &DbError, pos: Option<String>) -> String {
    let mut s = db.message().to_string();
    if let Some(d) = db.detail() {
        s.push_str("\nDETAIL: ");
        s.push_str(d);
    }
    if let Some(h) = db.hint() {
        s.push_str("\nHINT: ");
        s.push_str(h);
    }
    if let Some(p) = pos {
        s.push('\n');
        s.push_str(&p);
    }
    s
}

fn other_text(e: &tokio_postgres::Error) -> String {
    if e.is_closed() {
        return "The connection to the server was lost. OurDB reconnects on the next call.".into();
    }
    let mut s = e.to_string();
    let mut cause = std::error::Error::source(e);
    while let Some(c) = cause {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cause = c.source();
    }
    s
}

/// Line and column (both from 1) of character `pos` (from 1) of the statement
/// that starts at byte `offset` of `text`.
pub fn line_col(text: &str, offset: usize, pos: u32) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = text.get(..offset).map_or(0, |s| s.chars().count());
    let index = before + (pos as usize).saturating_sub(1);
    let (mut line, mut col) = (1, 1);
    for ch in text.chars().take(index) {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssl_options_are_removed() {
        assert_eq!(
            without_ssl_options(
                "postgres://u:p@h/db?sslmode=verify-full&sslrootcert=/x.pem&connect_timeout=5"
            ),
            "postgres://u:p@h/db?connect_timeout=5"
        );
        assert_eq!(
            without_ssl_options("postgresql://u:p?w@h/db?sslmode=require"),
            "postgresql://u:p?w@h/db"
        );
        assert_eq!(without_ssl_options("postgres://h/db"), "postgres://h/db");
        assert_eq!(
            without_ssl_options("host=h sslmode=require"),
            "host=h sslmode=require"
        );
    }

    #[test]
    fn positions() {
        let text = "select 1;\nselect nope\n  from t";
        let offset = text.find("select nope").unwrap();
        assert_eq!(line_col(text, offset, 8), (2, 8));
        assert_eq!(line_col("a\nbé c", 0, 6), (2, 4));
    }

    #[test]
    fn config_defaults() {
        let cfg = ConnConfig {
            user: "u".into(),
            ..Default::default()
        };
        let c = build_config(&cfg).unwrap();
        assert_eq!(c.get_dbname(), Some("postgres"));
        assert_eq!(c.get_application_name(), Some("OurDB"));
        assert_eq!(c.get_ports(), &[5432]);
        let url = ConnConfig {
            url: "postgres://a@h:6000/x?sslmode=verify-full".into(),
            password: "pw".into(),
            ..cfg
        };
        let c = build_config(&url).unwrap();
        assert_eq!(c.get_dbname(), Some("x"));
        assert_eq!(c.get_password(), Some(&b"pw"[..]));
        assert_eq!(c.get_ssl_mode(), SslMode::Prefer);
    }
}
