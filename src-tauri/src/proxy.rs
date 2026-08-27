//! Local loopback router.
//!
//! Chromium/Discord reaches us via the system PAC as an HTTP proxy. We:
//!   - serve the PAC on `GET /proxy.pac`
//!   - tunnel `CONNECT host:port` requests
//!
//! Only Discord gateway hosts (`*.discord.gg`) are sent through the non-BR exit;
//! that is the connection whose origin IP the Go Live / camera region gate reads.
//! Everything else goes direct. If the exit can't be reached within the timeout,
//! we fall back to a direct connection so Discord always opens (the fallback lives
//! here, never in the PAC, so Chromium can't silently prefer DIRECT).

use crate::state::{ExitInfo, Shared};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_socks::tcp::Socks5Stream;

/// How long to try the chosen exit before failing open to a direct connection,
/// so a slow/dead exit never leaves Discord stuck "connecting" / logged out.
const EXIT_TIMEOUT: Duration = Duration::from_secs(6);
/// If no exit is ready yet, HOLD a gateway connection this long waiting for one
/// (so it's born routed) before failing open to direct.
const HOLD_DEADLINE: Duration = Duration::from_secs(12);

pub fn pac_body(port: u16) -> String {
    format!(
        "function FindProxyForURL(url, host){{\n  \
if (host == \"gateway.discord.gg\" || dnsDomainIs(host, \".discord.gg\"))\n    \
return \"PROXY 127.0.0.1:{port}\";\n  \
return \"DIRECT\";\n}}"
    )
}

pub async fn run_router(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let addr = format!("127.0.0.1:{}", shared.port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            shared.set_status(format!("falha ao abrir a porta {addr}: {e}"));
            return;
        }
    };
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            accepted = listener.accept() => {
                if let Ok((sock, _)) = accepted {
                    let sh = shared.clone();
                    tokio::spawn(async move { let _ = handle_conn(sock, sh).await; });
                }
            }
        }
    }
}

async fn handle_conn(mut client: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    // Read the request line (and any headers) up to the header terminator.
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        let n = client.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16384 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let first = head.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = match target.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(443)),
            None => (target.to_string(), 443),
        };
        handle_connect(client, shared, host, port).await
    } else if method.eq_ignore_ascii_case("GET") && target.starts_with("/proxy.pac") {
        let body = pac_body(shared.port);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(resp.as_bytes()).await?;
        Ok(())
    } else {
        let _ = client
            .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
            .await;
        Ok(())
    }
}

enum Upstream {
    Socks(Socks5Stream<TcpStream>),
    Direct(TcpStream),
}

async fn handle_connect(
    mut client: TcpStream,
    shared: Arc<Shared>,
    host: String,
    port: u16,
) -> std::io::Result<()> {
    let is_gateway = host == "gateway.discord.gg" || host.ends_with(".discord.gg");
    // Always route the gateway through the exit (so Go Live survives reconnects).
    // If no exit is ready yet, HOLD briefly so the connection is born routed.
    let exit = if is_gateway {
        match shared.get_exit() {
            Some(e) => Some(e),
            None => wait_for_exit(&shared, HOLD_DEADLINE).await,
        }
    } else {
        None
    };

    // Try the exit; if it's slow or dead, FAIL OPEN to a direct connection (so
    // Discord never gets stuck / logged out) and trigger a fresh pool validation.
    let upstream: Option<Upstream> = match exit {
        Some(e) => {
            match tokio::time::timeout(
                EXIT_TIMEOUT,
                Socks5Stream::connect(e.addr.as_str(), (host.as_str(), port)),
            )
            .await
            {
                Ok(Ok(s)) => {
                    crate::log::log(&format!(
                        "router: gateway {host} -> exit {} ({})",
                        e.ip, e.country
                    ));
                    Some(Upstream::Socks(s))
                }
                _ => {
                    crate::log::log(&format!(
                        "router: exit {} lento/morto -> DIRECT + revalidar",
                        e.addr
                    ));
                    shared.set_exit(None);
                    shared.refresh_now.notify_one();
                    connect_direct(&host, port).await.map(Upstream::Direct)
                }
            }
        }
        None => {
            if is_gateway {
                crate::log::log("router: gateway -> DIRECT (sem saida a tempo)");
            }
            connect_direct(&host, port).await.map(Upstream::Direct)
        }
    };

    match upstream {
        Some(up) => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            match up {
                Upstream::Socks(mut s) => {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut s).await;
                }
                Upstream::Direct(mut s) => {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut s).await;
                }
            }
        }
        None => {
            let _ = client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                .await;
        }
    }
    Ok(())
}

async fn connect_direct(host: &str, port: u16) -> Option<TcpStream> {
    TcpStream::connect((host, port)).await.ok()
}

/// Poll for a ready exit up to `deadline` so the gateway is born routed.
/// Returns None if it times out or the app is deactivated mid-wait.
async fn wait_for_exit(shared: &Arc<Shared>, deadline: Duration) -> Option<ExitInfo> {
    let start = Instant::now();
    loop {
        if let Some(e) = shared.get_exit() {
            return Some(e);
        }
        if !shared.active.load(Ordering::SeqCst) || start.elapsed() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// (routing is now always-on for the gateway; bootstrap window removed)
