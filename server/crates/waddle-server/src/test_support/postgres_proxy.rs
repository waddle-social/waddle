//! Local-only plaintext transports for disposable PostgreSQL fault-injection tests.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use sqlx::postgres::PgConnectOptions;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

pub trait PostgresProxyStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> PostgresProxyStream for T {}

fn socket_directory(options: &PgConnectOptions) -> Option<PathBuf> {
    options.get_socket().cloned().or_else(|| {
        let host = Path::new(options.get_host());
        host.is_absolute().then(|| host.to_owned())
    })
}

pub fn ensure_local_target(options: &PgConnectOptions) -> io::Result<()> {
    if let Some(socket) = socket_directory(options) {
        return if socket.is_absolute() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "PostgreSQL socket path must be absolute",
            ))
        };
    }
    let host = options.get_host();
    if host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "plaintext PostgreSQL proxy requires a local target",
        ))
    }
}

pub fn ensure_loopback_addresses(addresses: &[SocketAddr]) -> io::Result<()> {
    if addresses.is_empty() || addresses.iter().any(|address| !address.ip().is_loopback()) {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "PostgreSQL proxy resolved outside loopback",
        ))
    } else {
        Ok(())
    }
}

pub async fn connect_local_postgres(
    options: &PgConnectOptions,
) -> io::Result<Box<dyn PostgresProxyStream>> {
    ensure_local_target(options)?;
    if let Some(socket) = socket_directory(options) {
        let path = socket.join(format!(".s.PGSQL.{}", options.get_port()));
        return Ok(Box::new(UnixStream::connect(path).await?));
    }
    let addresses: Vec<_> = tokio::net::lookup_host((options.get_host(), options.get_port()))
        .await?
        .collect();
    ensure_loopback_addresses(&addresses)?;
    let stream = TcpStream::connect(addresses.as_slice()).await?;
    ensure_loopback_addresses(&[stream.peer_addr()?])?;
    // Proxies forward each frame as separate tag, length and body writes.
    // Without TCP_NODELAY every forwarded frame waits for the peer's delayed
    // acknowledgement, which a Unix-socket upstream never imposes.
    stream.set_nodelay(true)?;
    Ok(Box::new(stream))
}

pub fn local_proxy_url(value: &str, port: u16) -> io::Result<url::Url> {
    let mut url = url::Url::parse(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    url.set_host(Some("127.0.0.1"))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    url.set_port(Some(port))
        .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "invalid proxy URL port"))?;
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| {
            !matches!(
                key.as_ref(),
                "host"
                    | "hostaddr"
                    | "port"
                    | "sslmode"
                    | "ssl-mode"
                    | "sslrootcert"
                    | "ssl-root-cert"
                    | "ssl-ca"
                    | "sslcert"
                    | "ssl-cert"
                    | "sslkey"
                    | "ssl-key"
            )
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("sslmode", "disable");
    Ok(url)
}
