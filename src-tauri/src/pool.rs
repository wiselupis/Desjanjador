//! Free-proxy pool: fetch, rank, and validate SOCKS5 exits.
//!
//! Validation is also our MITM/credential defense. For each candidate we make a
//! real TLS request to Cloudflare's trace through the proxy. Success proves, in
//! one shot: the tunnel works, the certificate is valid (schannel would reject a
//! MITM), and it reveals the true exit IP + country. We only accept non-BR exits
//! and prefer privacy-friendly countries (US first). Port 4145 is dropped — it is
//! overwhelmingly SOCKS4 relays that intercept TLS.

use crate::state::{ExitInfo, Shared};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

/// Country preference order (best first). Anything non-BR that validates is
/// still accepted, just ranked lower.
const PREFERRED: &[&str] = &[
    "US", "CA", "NL", "CH", "DE", "SE", "GB", "FR", "FI", "IE", "IS", "LU", "NO", "AT",
];

const VALIDATE_TIMEOUT: Duration = Duration::from_secs(12);
const BATCH: usize = 12;

struct Cand {
    addr: String,
    country: String,
    alive: bool,
    uptime: f64,
    timeout: f64,
}

/// Fetch + validate; publishes the best exit into shared state as it goes.
pub async fn refresh_pool(shared: Arc<Shared>) {
    crate::log::log("pool: refresh start");
    shared.set_status("procurando saída fora do Brasil (Tor + proxies)…");

    // Race Tor detection and free-proxy validation concurrently; the first valid
    // exit wins and is published immediately (the router's hold picks it up).
    let tor_task = {
        let sh = shared.clone();
        tokio::spawn(async move {
            match try_tor().await {
                Some(tor) => publish_if_first(&sh, tor, "Tor"),
                None => crate::log::log("pool: no local Tor reachable"),
            }
        })
    };
    let free_task = {
        let sh = shared.clone();
        tokio::spawn(async move { free_search(sh).await })
    };
    let _ = tokio::join!(tor_task, free_task);

    if shared.get_exit().is_none() {
        crate::log::log("pool: NO exit found -> gateway goes DIRECT (Go Live stays blocked)");
        shared.set_status("sem saída fora do Brasil — abra o Tor Browser para estabilidade");
    }
}

/// Publish an exit if we don't have one yet, or upgrade to a US exit.
fn publish_if_first(shared: &Shared, e: ExitInfo, via: &str) {
    let mut guard = shared.exit.lock().unwrap();
    let take = match &*guard {
        None => true,
        Some(cur) => e.country == "US" && cur.country != "US",
    };
    if take {
        *guard = Some(e.clone());
        drop(guard);
        crate::log::log(&format!("pool: exit selected via {via}: {} ({})", e.ip, e.country));
        shared.set_status(format!("saída pronta: {} · {} (via {})", e.country, e.ip, via));
    }
}

/// Fetch + validate free proxies in batches, publishing each success as it lands.
async fn free_search(shared: Arc<Shared>) {
    let cands = fetch_candidates().await;
    crate::log::log(&format!("pool: fetched {} free candidates", cands.len()));
    if cands.is_empty() {
        return;
    }
    for batch in cands.chunks(BATCH) {
        // Stop once we already have the ideal (US) exit.
        if let Some(e) = shared.get_exit() {
            if e.country == "US" {
                break;
            }
        }
        let mut handles = Vec::new();
        for c in batch {
            let addr = c.addr.clone();
            let sh = shared.clone();
            handles.push(tokio::spawn(async move {
                if let Some(info) = validate(&addr).await {
                    publish_if_first(&sh, info, "free");
                }
            }));
        }
        for h in handles {
            let _ = h.await;
        }
    }
}

/// If a local Tor SOCKS port is up (Tor Browser 9150 or daemon 9050) and its
/// exit is non-BR, return it as a preferred exit.
async fn try_tor() -> Option<ExitInfo> {
    for port in [9150u16, 9050u16, 9060u16, 9052u16, 9250u16] {
        let addr = format!("127.0.0.1:{port}");
        let reachable = tokio::time::timeout(Duration::from_millis(500), TcpStream::connect(&addr))
            .await
            .ok()
            .and_then(|r| r.ok())
            .is_some();
        if !reachable {
            continue;
        }
        if let Some(mut info) = validate(&addr).await {
            info.country = if info.country.is_empty() {
                "Tor".into()
            } else {
                format!("Tor·{}", info.country)
            };
            return Some(info);
        }
    }
    None
}

/// Validate one SOCKS5 proxy by doing a TLS request to Cloudflare's trace.
async fn validate(addr: &str) -> Option<ExitInfo> {
    let proxy = reqwest::Proxy::all(format!("socks5h://{addr}")).ok()?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(VALIDATE_TIMEOUT)
        .build()
        .ok()?;
    let resp = client
        .get("https://www.cloudflare.com/cdn-cgi/trace")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    let mut ip = String::new();
    let mut loc = String::new();
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("ip=") {
            ip = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("loc=") {
            loc = v.trim().to_string();
        }
    }
    if loc.is_empty() || loc == "BR" {
        return None; // must be a real, non-Brazilian exit
    }
    Some(ExitInfo {
        addr: addr.to_string(),
        ip,
        country: loc,
    })
}

/// Fetch candidates from ProxyScrape, falling back to Geonode. Returns a ranked
/// list (alive + US + high uptime + low timeout first), 4145 dropped.
async fn fetch_candidates() -> Vec<Cand> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // Query both providers in parallel and merge (dedup by addr) for speed + reach.
    let (ps, gn) = tokio::join!(fetch_proxyscrape(&client), fetch_geonode(&client));
    crate::log::log(&format!(
        "pool: sources proxyscrape={} geonode={}",
        ps.len(),
        gn.len()
    ));
    let mut cands = ps;
    cands.extend(gn);
    {
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| seen.insert(c.addr.clone()));
    }

    // Drop 4145 (SOCKS4 TLS-interception hotspot) and dead entries with no data.
    cands.retain(|c| !c.addr.ends_with(":4145"));

    cands.sort_by(|a, b| {
        let ap = PREFERRED.iter().position(|c| *c == a.country).unwrap_or(99);
        let bp = PREFERRED.iter().position(|c| *c == b.country).unwrap_or(99);
        b.alive
            .cmp(&a.alive)
            .then(ap.cmp(&bp))
            .then(b.uptime.partial_cmp(&a.uptime).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.timeout.partial_cmp(&b.timeout).unwrap_or(std::cmp::Ordering::Equal))
    });
    cands.truncate(72);
    cands
}

async fn fetch_proxyscrape(client: &reqwest::Client) -> Vec<Cand> {
    let url = "https://api.proxyscrape.com/v4/free-proxy-list/get?request=display_proxies&proxy_format=protocolipport&format=json&protocol=socks5";
    let val: serde_json::Value = match client.get(url).send().await {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let arr = val
        .get("proxies")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for p in arr {
        let ip = p.get("ip").and_then(|v| v.as_str()).unwrap_or("");
        let port = p.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
        if ip.is_empty() || port == 0 {
            continue;
        }
        let country = p
            .get("ip_data")
            .and_then(|d| d.get("countryCode"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(Cand {
            addr: format!("{ip}:{port}"),
            country,
            alive: p.get("alive").and_then(|v| v.as_bool()).unwrap_or(true),
            uptime: p.get("uptime").and_then(|v| v.as_f64()).unwrap_or(0.0),
            timeout: p
                .get("timeout")
                .and_then(|v| v.as_f64())
                .or_else(|| p.get("average_timeout").and_then(|v| v.as_f64()))
                .unwrap_or(9999.0),
        });
    }
    out
}

async fn fetch_geonode(client: &reqwest::Client) -> Vec<Cand> {
    let url = "https://proxylist.geonode.com/api/proxy-list?limit=200&page=1&sort_by=lastChecked&sort_type=desc&protocols=socks5";
    let val: serde_json::Value = match client.get(url).send().await {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let arr = val
        .get("data")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for p in arr {
        let ip = p.get("ip").and_then(|v| v.as_str()).unwrap_or("");
        let port = p
            .get("port")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| p.get("port").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        if ip.is_empty() || port == 0 {
            continue;
        }
        out.push(Cand {
            addr: format!("{ip}:{port}"),
            country: p
                .get("country")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            alive: true,
            uptime: p.get("upTime").and_then(|v| v.as_f64()).unwrap_or(0.0),
            timeout: p
                .get("responseTime")
                .and_then(|v| v.as_f64())
                .unwrap_or(9999.0),
        });
    }
    out
}
