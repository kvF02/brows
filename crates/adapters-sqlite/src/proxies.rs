use crate::migration::{migrate, sqlite_error};
use brows_application::{PortError, PortErrorKind, PortResult, ProxyRepository};
use brows_domain::{DnsMode, ProxyId, ProxyScheme, ProxySpec, SecretRef};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

#[derive(Debug)]
pub struct SqliteProxyRepository {
    connection: Mutex<Connection>,
}

impl SqliteProxyRepository {
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| sqlite_error("open proxy database", error))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| sqlite_error("configure proxy database timeout", error))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| sqlite_error("enable proxy foreign keys", error))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| sqlite_error("enable proxy WAL", error))?;
        connection
            .pragma_update(None, "trusted_schema", "OFF")
            .map_err(|error| sqlite_error("disable trusted proxy schema", error))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> PortResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            PortError::new(PortErrorKind::Unavailable, "proxy database is unavailable")
        })
    }
}

impl ProxyRepository for SqliteProxyRepository {
    fn list(&self) -> PortResult<Vec<ProxySpec>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT id,name,scheme,host,port,username,secret_ref,dns_mode,connect_timeout_ms FROM proxies ORDER BY name COLLATE NOCASE,id")
            .map_err(|error| sqlite_error("prepare proxy list", error))?;
        let rows = statement
            .query_map([], from_row)
            .map_err(|error| sqlite_error("query proxies", error))?;
        let mut proxies = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| sqlite_error("read proxies", error))?;
        for proxy in &mut proxies {
            proxy.bypass_hosts = bypass_hosts(&connection, &proxy.id)?;
        }
        Ok(proxies)
    }

    fn get(&self, id: &ProxyId) -> PortResult<ProxySpec> {
        let connection = self.lock()?;
        let mut proxy = connection.query_row("SELECT id,name,scheme,host,port,username,secret_ref,dns_mode,connect_timeout_ms FROM proxies WHERE id=?1", [id.as_str()], from_row)
            .optional().map_err(|error| sqlite_error("get proxy", error))?.ok_or_else(|| PortError::not_found("proxy"))?;
        proxy.bypass_hosts = bypass_hosts(&connection, id)?;
        Ok(proxy)
    }

    fn save(&self, proxy: &ProxySpec) -> PortResult<()> {
        proxy
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let mut connection = self.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_error("start proxy save", error))?;
        tx.execute("INSERT INTO proxies(id,name,scheme,host,port,username,secret_ref,dns_mode,connect_timeout_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(id) DO UPDATE SET name=excluded.name,scheme=excluded.scheme,host=excluded.host,port=excluded.port,username=excluded.username,secret_ref=excluded.secret_ref,dns_mode=excluded.dns_mode,connect_timeout_ms=excluded.connect_timeout_ms",
            params![proxy.id.as_str(),proxy.name,scheme(proxy.scheme),proxy.host,proxy.port,proxy.username,proxy.secret_ref.as_ref().map(SecretRef::as_str),dns(proxy.dns_mode),proxy.connect_timeout_ms])
            .map_err(|error| sqlite_error("save proxy", error))?;
        tx.execute(
            "DELETE FROM proxy_bypass_hosts WHERE proxy_id=?1",
            [proxy.id.as_str()],
        )
        .map_err(|error| sqlite_error("clear proxy bypass hosts", error))?;
        for (position, host) in proxy.bypass_hosts.iter().enumerate() {
            tx.execute(
                "INSERT INTO proxy_bypass_hosts(proxy_id,position,host) VALUES(?1,?2,?3)",
                params![proxy.id.as_str(), position as i64, host],
            )
            .map_err(|error| sqlite_error("save proxy bypass host", error))?;
        }
        tx.commit()
            .map_err(|error| sqlite_error("commit proxy save", error))
    }

    fn delete(&self, id: &ProxyId) -> PortResult<()> {
        let connection = self.lock()?;
        let in_use: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profiles WHERE proxy_id=?1)",
                [id.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_error("check proxy references", error))?;
        if in_use {
            return Err(PortError::new(
                PortErrorKind::Locked,
                "proxy is assigned to a profile",
            ));
        }
        let changed = connection
            .execute("DELETE FROM proxies WHERE id=?1", [id.as_str()])
            .map_err(|error| sqlite_error("delete proxy", error))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(PortError::not_found("proxy"))
        }
    }
}

fn bypass_hosts(connection: &Connection, id: &ProxyId) -> PortResult<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT host FROM proxy_bypass_hosts WHERE proxy_id=?1 ORDER BY position")
        .map_err(|error| sqlite_error("prepare proxy bypass hosts", error))?;
    let rows = statement
        .query_map([id.as_str()], |row| row.get(0))
        .map_err(|error| sqlite_error("query proxy bypass hosts", error))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("read proxy bypass hosts", error))
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<ProxySpec> {
    let id = ProxyId::new(row.get::<_, String>(0)?).map_err(convert(0))?;
    let secret_ref = row
        .get::<_, Option<String>>(6)?
        .map(SecretRef::new)
        .transpose()
        .map_err(convert(6))?;
    Ok(ProxySpec {
        id,
        name: row.get(1)?,
        scheme: parse_scheme(&row.get::<_, String>(2)?)?,
        host: row.get(3)?,
        port: row.get(4)?,
        username: row.get(5)?,
        secret_ref,
        dns_mode: parse_dns(&row.get::<_, String>(7)?)?,
        bypass_hosts: Vec::new(),
        connect_timeout_ms: row.get(8)?,
    })
}

fn convert(index: usize) -> impl FnOnce(brows_domain::DomainError) -> rusqlite::Error {
    move |error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    }
}
fn scheme(value: ProxyScheme) -> &'static str {
    match value {
        ProxyScheme::Http => "http",
        ProxyScheme::Https => "https",
        ProxyScheme::Socks5 => "socks5",
    }
}
fn dns(value: DnsMode) -> &'static str {
    match value {
        DnsMode::Proxy => "proxy",
        DnsMode::Local => "local",
    }
}
fn parse_scheme(value: &str) -> rusqlite::Result<ProxyScheme> {
    match value {
        "http" => Ok(ProxyScheme::Http),
        "https" => Ok(ProxyScheme::Https),
        "socks5" => Ok(ProxyScheme::Socks5),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn parse_dns(value: &str) -> rusqlite::Result<DnsMode> {
    match value {
        "proxy" => Ok(DnsMode::Proxy),
        "local" => Ok(DnsMode::Local),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
