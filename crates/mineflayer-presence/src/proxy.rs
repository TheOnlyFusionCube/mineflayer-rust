//! The proxies the Java runner accepts (SOCKS5, SOCKS4/4a, HTTP CONNECT), as
//! a TCP stream to the game server through the proxy.

use std::{io, net::SocketAddr};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Socks5,
    Socks4,
    Http,
}

#[derive(Clone, Debug)]
pub struct Proxy {
    pub kind: ProxyKind,
    /// `host:port` of the proxy.
    pub address: String,
    /// Username and password (SOCKS5, HTTP), or the SOCKS4 user id.
    pub credentials: Option<(String, String)>,
}

impl Proxy {
    /// Parse `socks5://user:pass@host:port`, `socks4://user@host:port` or
    /// `http://user:pass@host:port`.
    pub fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" => ProxyKind::Socks5,
            "socks4" | "socks4a" => ProxyKind::Socks4,
            "http" => ProxyKind::Http,
            _ => return None,
        };
        let (credentials, address) = match rest.rsplit_once('@') {
            Some((auth, address)) => {
                let (user, pass) = auth.split_once(':').unwrap_or((auth, ""));
                (Some((user.to_owned(), pass.to_owned())), address)
            }
            None => (None, rest),
        };
        let address = address.trim_end_matches('/');
        address.rsplit_once(':')?.1.parse::<u16>().ok()?;
        Some(Self { kind, address: address.to_owned(), credentials })
    }

    /// A TCP stream to `target` through this proxy.
    pub async fn connect(&self, target: SocketAddr) -> io::Result<TcpStream> {
        let mut s = TcpStream::connect(&self.address).await?;
        s.set_nodelay(true)?;
        match self.kind {
            ProxyKind::Socks5 => socks5(&mut s, target, self.credentials.as_ref()).await?,
            ProxyKind::Socks4 => socks4(&mut s, target, self.credentials.as_ref().map(|c| c.0.as_str())).await?,
            ProxyKind::Http => http(&mut s, target, self.credentials.as_ref()).await?,
        }
        Ok(s)
    }
}

fn refused(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionRefused, what.into())
}

fn address_bytes(target: SocketAddr) -> Vec<u8> {
    match target {
        SocketAddr::V4(a) => [&[1u8][..], &a.ip().octets()[..], &a.port().to_be_bytes()[..]].concat(),
        SocketAddr::V6(a) => [&[4u8][..], &a.ip().octets()[..], &a.port().to_be_bytes()[..]].concat(),
    }
}

async fn socks5(s: &mut TcpStream, target: SocketAddr, credentials: Option<&(String, String)>) -> io::Result<()> {
    const NO_AUTH: u8 = 0;
    const USER_PASS: u8 = 2;
    let methods: &[u8] = if credentials.is_some() { &[NO_AUTH, USER_PASS] } else { &[NO_AUTH] };
    s.write_all(&[&[5u8, methods.len() as u8][..], methods].concat()).await?;
    let mut choice = [0u8; 2];
    s.read_exact(&mut choice).await?;
    match (choice, credentials) {
        ([5, NO_AUTH], _) => {}
        ([5, USER_PASS], Some((user, pass))) => {
            if user.len() > 255 || pass.len() > 255 {
                return Err(refused("SOCKS5 credentials longer than 255 bytes"));
            }
            let mut auth = vec![1, user.len() as u8];
            auth.extend(user.as_bytes());
            auth.push(pass.len() as u8);
            auth.extend(pass.as_bytes());
            s.write_all(&auth).await?;
            let mut status = [0u8; 2];
            s.read_exact(&mut status).await?;
            if status[1] != 0 {
                return Err(refused("SOCKS5 proxy rejected the credentials"));
            }
        }
        _ => return Err(refused(format!("SOCKS5 proxy offered no usable method ({choice:?})"))),
    }
    s.write_all(&[&[5u8, 1, 0][..], &address_bytes(target)].concat()).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0 {
        return Err(refused(format!("SOCKS5 proxy refused the connection (reply {})", head[1])));
    }
    let rest = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await?;
            usize::from(n[0]) + 2
        }
        t => return Err(refused(format!("SOCKS5 reply with address type {t}"))),
    };
    let mut bound = vec![0u8; rest];
    s.read_exact(&mut bound).await.map(|_| ())
}

async fn socks4(s: &mut TcpStream, target: SocketAddr, user: Option<&str>) -> io::Result<()> {
    let SocketAddr::V4(target) = target else {
        return Err(refused("SOCKS4 reaches only IPv4 addresses"));
    };
    let mut request = vec![4u8, 1];
    request.extend(target.port().to_be_bytes());
    request.extend(target.ip().octets());
    request.extend(user.unwrap_or("").as_bytes());
    request.push(0);
    s.write_all(&request).await?;
    let mut reply = [0u8; 8];
    s.read_exact(&mut reply).await?;
    if reply[1] != 90 {
        return Err(refused(format!("SOCKS4 proxy refused the connection (reply {})", reply[1])));
    }
    Ok(())
}

async fn http(s: &mut TcpStream, target: SocketAddr, credentials: Option<&(String, String)>) -> io::Result<()> {
    use base64::Engine;
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some((user, pass)) = credentials {
        request.push_str(&format!("Proxy-Authorization: Basic {}\r\n", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))));
    }
    request.push_str("\r\n");
    s.write_all(request.as_bytes()).await?;
    // the response head ends at the first blank line; read it a byte at a
    // time so nothing of the tunnel that follows is consumed
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 {
            return Err(refused("HTTP proxy response head too long"));
        }
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await?;
        head.push(b[0]);
    }
    let status = String::from_utf8_lossy(&head);
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if code != "200" {
        return Err(refused(format!("HTTP proxy answered {}", status.lines().next().unwrap_or(""))));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_kinds_with_and_without_credentials() {
        let p = Proxy::parse("socks5://alice:s3cret@10.0.0.1:1080").unwrap();
        assert_eq!((p.kind, p.address.as_str()), (ProxyKind::Socks5, "10.0.0.1:1080"));
        assert_eq!(p.credentials, Some(("alice".into(), "s3cret".into())));
        assert_eq!(Proxy::parse("socks4://bob@proxy.example:1080").unwrap().kind, ProxyKind::Socks4);
        assert!(Proxy::parse("http://proxy.example:3128").unwrap().credentials.is_none());
        assert!(Proxy::parse("ftp://x:1").is_none());
        assert!(Proxy::parse("socks5://no-port").is_none());
    }
}
