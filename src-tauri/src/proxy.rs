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

/// Per-attempt cap on dialing the exit's SOCKS tunnel, so a slow/dead exit never
/// leaves Discord stuck "connecting" / logged out.
const EXIT_TIMEOUT: Duration = Duration::from_secs(6);
/// If no exit is ready yet, HOLD a gateway connection at most this long waiting
/// for one (so it's born routed) before failing open to direct.
const HOLD_DEADLINE: Duration = Duration::from_secs(8);
/// Soft overall budget for routing ONE gateway CONNECT (across both attempts +
/// any hold) before failing open to a DIRECT connection. It bounds the WAITS; the
/// final dial and the direct fallback each keep a MIN_DIAL floor (so a fresh exit
/// / the fail-open still get a fair shot), so the true worst-case hold is
/// ~GATEWAY_DEADLINE + 2*MIN_DIAL (~16s). That still keeps a stale-exit retry from
/// holding Discord anywhere near indefinitely (the pre-fix path could exceed 20s).
const GATEWAY_DEADLINE: Duration = Duration::from_secs(12);
/// Cap the direct fallback connect too, so a blackholed route can't hang forever.
const DIRECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Floor for any single connect attempt. Near the overall deadline a freshly
/// found exit must still get a fair window (a live proxy connects well within
/// this), so it's never starved to a sub-second timeout and then wrongly cleared.
const MIN_DIAL: Duration = Duration::from_secs(2);

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
    let upstream: Option<Upstream> = if is_gateway {
        gateway_upstream(&shared, &host, port).await
    } else {
        connect_direct(&host, port, DIRECT_TIMEOUT)
            .await
            .map(Upstream::Direct)
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

/// Route a Discord gateway connection through a live non-BR exit.
///
/// Tries the current exit; if it's slow/dead, drops it, kicks a pool refresh, and
/// waits briefly for a fresh exit to try instead. Only if no exit can carry the
/// connection does it fail open to a DIRECT connection — so Discord always
/// connects (even if Go Live stays blocked that session) and, crucially, a stale
/// exit after a long idle no longer silently drops the tunnel: we swap in a fresh
/// one on the reconnect itself.
async fn gateway_upstream(shared: &Arc<Shared>, host: &str, port: u16) -> Option<Upstream> {
    let overall = Instant::now() + GATEWAY_DEADLINE;
    for _ in 0..2u8 {
        let left = overall.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        // Use the ready exit, or (if none) HOLD briefly for one so the connection
        // is born routed rather than direct — bounded by the overall budget.
        let exit = match shared.get_exit() {
            Some(e) => Some(e),
            None => wait_for_exit(shared, left.min(HOLD_DEADLINE)).await,
        };
        let e = match exit {
            Some(e) => e,
            None => break, // no exit available in time -> direct
        };
        // Give the dial a FAIR window even near the deadline (MIN_DIAL floor): a
        // live proxy connects well within it, so a just-published exit is never
        // starved to a sub-second timeout and then wrongly cleared below.
        let left = overall.saturating_duration_since(Instant::now());
        let dial = left.min(EXIT_TIMEOUT).max(MIN_DIAL);
        match tokio::time::timeout(
            dial,
            Socks5Stream::connect(e.addr.as_str(), (host, port)),
        )
        .await
        {
            Ok(Ok(s)) => {
                crate::log::log(&format!(
                    "router: gateway {host} -> exit {} ({})",
                    e.ip, e.country
                ));
                return Some(Upstream::Socks(s));
            }
            _ => {
                // Instant make-before-break: promote the warm, pre-validated backup
                // for THIS failed exit so the retry below routes through it with no
                // wait — but never promote the SAME exit that just failed.
                match shared.take_backup() {
                    Some(b) if b.addr != e.addr => {
                        if shared.replace_exit_if(&e.addr, b.clone()) {
                            crate::log::log(&format!(
                                "router: exit {} falhou -> promoveu reserva {} ({})",
                                e.addr, b.ip, b.country
                            ));
                        } else {
                            shared.set_backup(Some(b)); // health loop already swapped
                        }
                    }
                    _ => {
                        // No usable backup: clear ONLY this exit (leave a fresh one
                        // the health loop may have published) and search.
                        crate::log::log(&format!("router: exit {} lento/morto", e.addr));
                        shared.clear_exit_if(&e.addr);
                    }
                }
                shared.refresh_now.notify_one();
                // Next iteration: get_exit()/wait_for_exit picks up the replacement.
            }
        }
    }
    // Fail open to DIRECT, bounded by whatever budget remains (floored to MIN_DIAL
    // so a normal direct connect still completes), so the direct tail adds at most
    // ~MIN_DIAL rather than a full DIRECT_TIMEOUT on top of the loop budget.
    let direct_to = overall
        .saturating_duration_since(Instant::now())
        .min(DIRECT_TIMEOUT)
        .max(MIN_DIAL);
    crate::log::log("router: gateway -> DIRECT (sem saída viável)");
    connect_direct(host, port, direct_to).await.map(Upstream::Direct)
}

async fn connect_direct(host: &str, port: u16, timeout: Duration) -> Option<TcpStream> {
    match tokio::time::timeout(timeout, TcpStream::connect((host, port))).await {
        Ok(Ok(s)) => Some(s),
        _ => None,
    }
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
