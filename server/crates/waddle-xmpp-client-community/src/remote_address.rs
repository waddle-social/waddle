//! RFC 5122 and XEP-0503 addresses accepted by the federation directory.
use jid::BareJid;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use url::Host;

const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub jid: BareJid,
    pub node: Option<String>,
}

impl Address {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.len() > 8192 {
            return None;
        }
        let (path, node) = if raw
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("xmpp:"))
        {
            let uri = raw[5..].split('#').next()?;
            let uri = match uri.strip_prefix("//") {
                Some(rest) => rest.split_once('/')?.1,
                None => uri,
            };
            let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
            let mut node = None;
            for param in query.split(';').skip(1) {
                if let Some((key, value)) = param.split_once('=') {
                    if decode(key)? == "node" && node.is_none() {
                        node = Some(decode(value)?);
                    }
                }
            }
            (decode(path)?, node)
        } else {
            (raw.to_owned(), None)
        };
        Self::from_parts(&path, node.as_deref())
    }

    pub fn from_parts(path: &str, node: Option<&str>) -> Option<Self> {
        let (local, domain) = match path.split_once('@') {
            Some((local, domain))
                if !local.is_empty()
                    && !local
                        .chars()
                        .any(|c| c.is_whitespace() || "\"&'/:<>@".contains(c))
                    && node.is_none() =>
            {
                (Some(local), domain)
            }
            Some(_) => return None,
            None => (None, path),
        };
        // A DNS federation peer has at least two labels and a non-numeric final label.
        let Host::Domain(domain) = Host::parse(domain.strip_suffix('.').unwrap_or(domain)).ok()?
        else {
            return None;
        };
        if domain.len() > 253
            || !domain.contains('.')
            || domain.split('.').any(|s| {
                s.is_empty()
                    || s.len() > 63
                    || s.starts_with('-')
                    || s.ends_with('-')
                    || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || domain
                .rsplit('.')
                .next()?
                .bytes()
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        if node.is_some_and(|n| n.is_empty() || n.len() > 2048 || n.chars().any(char::is_control)) {
            return None;
        }
        let jid = match local {
            Some(l) => format!("{}@{domain}", l.to_lowercase()),
            None => domain,
        }
        .parse()
        .ok()?;
        Some(Self {
            jid,
            node: node.map(str::to_owned),
        })
    }

    pub fn text(&self) -> String {
        match &self.node {
            Some(n) => format!(
                "xmpp:{}?;node={}",
                self.jid,
                utf8_percent_encode(n, COMPONENT)
            ),
            None => self.jid.to_string(),
        }
    }
}

fn decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    if bytes.iter().enumerate().any(|(i, b)| {
        *b == b'%'
            && !bytes
                .get(i + 1..i + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    }) {
        return None;
    }
    percent_decode_str(input)
        .decode_utf8()
        .ok()
        .map(|x| x.into_owned())
}
