use std::net::SocketAddr;
use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

pub struct LoopbackTlsServer {
    pub addr: SocketAddr,
    pub ca_pem: Vec<u8>,
    pub listener: TcpListener,
    pub acceptor: TlsAcceptor,
}

impl LoopbackTlsServer {
    pub async fn bind(domain: &str) -> Self {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_key = KeyPair::generate().expect("ca key");
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca self signed");
        let ca_pem = ca_cert.pem().into_bytes();

        let leaf_params = CertificateParams::new(vec![domain.to_owned()]).expect("leaf params");
        let leaf_key = KeyPair::generate().expect("leaf key");
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .expect("leaf signed");

        let cert_der: CertificateDer<'static> = leaf_cert.der().clone();
        let ca_der: CertificateDer<'static> = ca_cert.der().clone();
        let key_der = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server_config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert_der, ca_der], key_der)
            .expect("server config");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        Self {
            addr,
            ca_pem,
            listener,
            acceptor,
        }
    }
}

pub async fn read_http_request(stream: &mut TlsStream<TcpStream>) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream.read(&mut tmp).await.expect("read tls request");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(headers_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf[..headers_end]);
            let content_len = headers
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    if k.eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if buf.len() >= headers_end + 4 + content_len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}
