//! OurDB: a simple database client. This binary only wires the UI to the drivers.

use std::sync::Arc;

use anyhow::bail;
use ourdb_core::{ConnConfig, Connector, Driver, Kind, Session, tunnel::Tunnel};

/// Open a session: start the SSH tunnel when one is set, then connect the driver.
async fn connect(mut cfg: ConnConfig) -> anyhow::Result<Session> {
    let tunnel = if !cfg.ssh.trim().is_empty() && cfg.kind != Kind::Sqlite {
        if cfg.has_url() {
            bail!("An SSH tunnel needs the host and port fields, not a URL");
        }
        let (dest, key) = (cfg.ssh.clone(), cfg.ssh_key.clone());
        let (host, port) = (cfg.host_or_default().to_string(), cfg.port_or_default());
        let t = tokio::task::spawn_blocking(move || Tunnel::open(&dest, Some(&key), &host, port))
            .await??;
        cfg.host = "127.0.0.1".into();
        cfg.port = t.local_port;
        cfg.via_tunnel = true;
        Some(t)
    } else {
        None
    };
    let driver: Arc<dyn Driver> = match cfg.kind {
        Kind::Postgres => ourdb_postgres::connect(&cfg).await?,
        Kind::MySql => ourdb_mysql::connect(&cfg).await?,
        Kind::Sqlite => ourdb_sqlite::connect(&cfg).await?,
        Kind::MongoDb | Kind::DocumentDb => ourdb_mongo::connect(&cfg).await?,
    };
    Ok(Session { driver, tunnel })
}

fn main() -> anyhow::Result<()> {
    let connector: Connector = Arc::new(|cfg| Box::pin(connect(cfg)));
    ourdb_ui::run(connector)
}
