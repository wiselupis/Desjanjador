//! Free-proxy pool: fetch, rank, and validate SOCKS5 exits.
//!
//! Validation is also our MITM/credential defense. For each candidate we make a
//! real TLS request to Cloudflare's trace through the proxy. Success proves, in
//! one shot: the tunnel works, the certificate is valid (schannel would reject a
//! MITM), and it reveals the true exit IP + country. We only accept non-BR exits
//! and prefer privacy-friendly countries (US first). Port 4145 is dropped — it is
//! overwhelmingly SOCKS4 relays that intercept TLS.

use crate::state::{ExitInfo, Shared};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

/// Country preference order (best first). Anything non-BR that validates is
/// still accepted, just ranked lower.
const PREFERRED: &[&str] = &[
    "US", "CA", "NL", "CH", "DE", "SE", "GB", "FR", "FI", "IE", "IS", "LU", "NO", "AT",
];

/// Timeout for DISCOVERY of a candidate (a full TLS request to Cloudflare through
/// the proxy — this is also our MITM/country check). Kept short so dead
/// candidates are rejected fast while scanning many.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(7);
/// GENEROUS timeout for the keep-warm HEALTH re-check of the one held exit. It
/// runs the SAME full-TLS validation (so it still catches an exit that accepts a
/// tunnel but mangles/RSTs the payload — a bare CONNECT check would miss that),
/// just with a lenient budget so a working-but-slow exit isn't dropped each cycle.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Consecutive failed health probes before the held exit is dropped. One blip is
/// tolerated (stability over churn); free proxies have very spiky latency.
const GRACE: u8 = 2;
/// Re-hit the free-proxy providers at most this often. Between fetches we
/// re-validate the cached candidate list, so a prolonged no-exit state never
/// hammers ProxyScrape/Geonode from the user's real IP (their APIs rate-limit).
const FETCH_TTL: Duration = Duration::from_secs(180);
const BATCH: usize = 16;

struct Cand {
    addr: String,
    country: String,
    alive: bool,
    uptime: f64,
    timeout: f64,
}

/// Loop-owned state for the keep-warm maintainer: health grace counter, the
/// cached provider candidate list (so we don't re-fetch every cycle), and the
/// last-working exit addr to re-try once on warm start.
#[derive(Default)]
pub struct Maint {
    fails: u8,
    cand_addrs: Vec<String>,
    fetched_at: Option<Instant>,
    /// Last working exit addr from a previous run; raced on the first discovery
    /// then cleared (`take`n) so it's only re-tried once.
    cached: Option<String>,
}

impl Maint {
    /// Seed with the last-working exit addr (from settings) for a warm start.
    pub fn new(cached: Option<String>) -> Self {
        Maint {
            cached,
            ..Default::default()
        }
    }
}

/// One maintenance pass, run on a ~30s cadence and whenever the router signals a
/// dead exit via `refresh_now`:
///   - if we hold an exit, health-probe it (with a small grace so one blip
///     doesn't churn a working exit);
///   - otherwise discover and publish a fresh one.
/// This is what keeps a LIVE exit ready before Discord next reconnects — without
/// it, a stale exit sits around until a gateway connection eats the router
/// timeout and falls open to a direct (blocked) connection.
pub async fn maintain(shared: Arc<Shared>, m: &mut Maint) {
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    if let Some(cur) = shared.get_exit() {
        if probe_exit(&cur.addr).await {
            m.fails = 0;
            return; // still carries the gateway — keep it
        }
        m.fails += 1;
        crate::log::log(&format!(
            "pool: probe da saída {} falhou ({}/{})",
            cur.addr, m.fails, GRACE
        ));
        if m.fails < GRACE {
            return; // tolerate a transient blip
        }
        m.fails = 0;
        if !shared.active.load(Ordering::SeqCst) {
            return; // deactivated while probing — don't touch state
        }
        crate::log::log(&format!("pool: saída {} caiu -> procurando outra", cur.addr));
        shared.clear_exit_if(&cur.addr);
        shared.set_status("saída caiu — procurando outra…");
    }
    discover(shared, m).await;
}

/// Find and publish a fresh exit: race local Tor against the (cached) free list.
async fn discover(shared: Arc<Shared>, m: &mut Maint) {
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    m.fails = 0; // the exit we're about to publish gets a fresh grace budget
    let t0 = Instant::now();
    crate::log::log("pool: procurando saída");
    shared.set_status("procurando uma saída rápida fora do Brasil…");

    // Refresh the candidate list from the providers at most every FETCH_TTL
    // (fetched_at=None counts as stale, so the first pass always fetches). Gate on
    // staleness ALONE — never on cand_addrs.is_empty() — so an EMPTY result
    // (providers rate-limited/down) does not force an immediate refetch next
    // cycle, which would feed the very rate-limit loop this cache exists to avoid.
    // An empty list simply means no exit this pass; we back off for FETCH_TTL.
    let stale = m.fetched_at.map_or(true, |t| t.elapsed() >= FETCH_TTL);
    if stale {
        let cands = fetch_candidates().await;
        crate::log::log(&format!("pool: {} candidatos (cache atualizado)", cands.len()));
        m.cand_addrs = cands.into_iter().map(|c| c.addr).collect();
        m.fetched_at = Some(Instant::now());
    }
    let addrs = m.cand_addrs.clone();

    // Race, all concurrently — the first valid exit wins and is published
    // immediately (the router picks it up); publish_if_first keeps whichever lands
    // first: (1) the cached last-working exit — usually the fastest, an instant
    // reconnect; (2) local Tor if present; (3) the free-proxy list.
    let cached_task = m.cached.take().map(|addr| {
        let sh = shared.clone();
        tokio::spawn(async move {
            if let Some(info) = validate(&addr, VALIDATE_TIMEOUT).await {
                publish_if_first(&sh, info, "cache");
            }
        })
    });
    let tor_task = {
        let sh = shared.clone();
        tokio::spawn(async move {
            match try_tor().await {
                Some(tor) => publish_if_first(&sh, tor, "Tor"),
                None => crate::log::log("pool: nenhum Tor local acessível"),
            }
        })
    };
    let free_task = {
        let sh = shared.clone();
        tokio::spawn(async move { free_search(sh, addrs).await })
    };
    let _ = tokio::join!(tor_task, free_task);
    if let Some(t) = cached_task {
        let _ = t.await;
    }

    // Remember the working exit so the next launch re-tries it instantly.
    if let Some(e) = shared.get_exit() {
        crate::log::log(&format!(
            "pool: saída pronta em {:.1}s",
            t0.elapsed().as_secs_f32()
        ));
        let dir = shared.config_dir.lock().unwrap().clone();
        crate::settings::save_last_exit(&dir, Some(e));
    } else if shared.active.load(Ordering::SeqCst) {
        crate::log::log("pool: nenhuma saída -> gateway vai DIRETO (Go Live bloqueado)");
        shared.set_status("sem saída no momento — tentando novamente…");
    }
}

/// Keep-warm liveness probe for the one held exit. Runs the SAME full-TLS
/// validation as discovery — so it catches an exit that still accepts a tunnel
/// but mangles/RSTs the payload (a bare CONNECT check would pass such a half-dead
/// exit forever) — just with a generous timeout so a slow-but-working exit is not
/// churned every cycle.
async fn probe_exit(addr: &str) -> bool {
    validate(addr, PROBE_TIMEOUT).await.is_some()
}

/// Publish an exit if we don't have one yet, or upgrade to a US exit.
fn publish_if_first(shared: &Shared, e: ExitInfo, via: &str) {
    let mut guard = shared.exit.lock().unwrap();
    // Re-check active WHILE holding the exit lock. deactivate() flips active=false
    // and then clears the exit under this same lock, so checking here serializes
    // the two: a validate task finishing after stop either sees inactive and skips,
    // or writes and deactivate's subsequent set_exit(None) removes it — never a
    // live exit left behind for a stopped app.
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
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

/// Validate the (already-ranked, cached) candidate addrs, keeping up to BATCH
/// checks in flight and STOPPING at the first success (aborting the rest) rather
/// than waiting for every straggler — so a working exit lands as fast as any one
/// proxy responds, not as slow as the slowest in a batch. Candidates are ranked
/// US-first, so the first success is usually US anyway.
async fn free_search(shared: Arc<Shared>, addrs: Vec<String>) {
    use tokio::task::JoinSet;
    if addrs.is_empty() {
        return;
    }
    let mut iter = addrs.into_iter();
    let mut set: JoinSet<()> = JoinSet::new();
    for _ in 0..BATCH {
        match iter.next() {
            Some(addr) => spawn_validate(&mut set, &shared, addr),
            None => break,
        }
    }
    while set.join_next().await.is_some() {
        if shared.get_exit().is_some() {
            set.abort_all();
            break; // someone published — stop racing
        }
        if let Some(addr) = iter.next() {
            spawn_validate(&mut set, &shared, addr); // keep the pipeline full
        }
    }
}

fn spawn_validate(set: &mut tokio::task::JoinSet<()>, shared: &Arc<Shared>, addr: String) {
    let sh = shared.clone();
    set.spawn(async move {
        if let Some(info) = validate(&addr, VALIDATE_TIMEOUT).await {
            publish_if_first(&sh, info, "free");
        }
    });
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
        if let Some(mut info) = validate(&addr, VALIDATE_TIMEOUT).await {
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

/// Validate one SOCKS5 proxy by doing a TLS request to Cloudflare's trace under
/// `timeout`. Success proves the tunnel + a valid certificate (no MITM) and
/// reveals the true exit IP/country.
async fn validate(addr: &str, timeout: Duration) -> Option<ExitInfo> {
    let proxy = reqwest::Proxy::all(format!("socks5h://{addr}")).ok()?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(timeout)
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

/// Fetch candidates from ProxyScrape + Geonode (in parallel, merged). Returns a
/// list ranked by reliability (alive + low latency + high uptime first), 4145
/// dropped, capped at 120.
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

    // Rank by RELIABILITY first (alive, then low latency, then high uptime) so we
    // hit a working proxy fast regardless of country; the privacy-friendly country
    // order is only a tiebreaker among equally-good ones. Any non-BR exit is
    // accepted (see validate), so this only decides who we try first.
    cands.sort_by(|a, b| {
        let ap = PREFERRED.iter().position(|c| *c == a.country).unwrap_or(99);
        let bp = PREFERRED.iter().position(|c| *c == b.country).unwrap_or(99);
        b.alive
            .cmp(&a.alive)
            .then(a.timeout.partial_cmp(&b.timeout).unwrap_or(std::cmp::Ordering::Equal))
            .then(b.uptime.partial_cmp(&a.uptime).unwrap_or(std::cmp::Ordering::Equal))
            .then(ap.cmp(&bp))
    });
    cands.truncate(120);
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
