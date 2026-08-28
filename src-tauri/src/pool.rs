//! Free-proxy pool: fetch, rank, validate, and keep two warm SOCKS5 exits.
//!
//! Validation is also our MITM/credential defense: for each candidate we make a
//! real TLS request to Cloudflare's trace through the proxy. Success proves in one
//! shot that the tunnel works, the certificate is valid (a MITM would be rejected),
//! and reveals the true exit IP + country. We accept an exit ONLY if its true
//! country is in ALLOWED. Among allowed exits we rank purely by reliability
//! (latency/uptime). Port 4145 is dropped (SOCKS4 relays that intercept TLS).
//!
//! A PRIMARY exit serves the router; a warm, pre-validated BACKUP is kept ready so
//! a dead/degraded primary is swapped instantly (make-before-break, no user gap).

use crate::state::{ExitInfo, Shared};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Countries safe to exit through: Discord voice/Go-Live AND age-restricted (NSFW)
/// channels work there with NO government block and NO IP-triggered age/ID
/// verification (the gateway's exit IP is Discord's effective jurisdiction), as of
/// Aug 2026 — researched + adversarially verified. Excludes blocked countries
/// (CN/RU/IR/UAE/EG/TR/…) and the IP-triggered age-verification ones: GB, AU, BR,
/// and the US (Texas/Utah have live age checks and an IP can't be pinned to a safe
/// state). Watch-items to revisit: GR's law takes effect 2027; DK/FR have pending
/// bills.
const ALLOWED: &[&str] = &[
    "TH", "FR", "DE", "IE", "IT", "NL", "BE", "PL", "CZ", "AT", "SE", "FI", "NO", "DK",
    "PT", "RO", "GR", "CH", "CA", "MX", "AR", "CL", "CO", "UY", "NZ", "JP", "TW", "HK",
    "IN", "PH", "SG", "IL", "ZA", "NG", "KE", "UA", "MD", "GE", "AM", "LK",
];

/// Timeout for DISCOVERY of a candidate (full TLS to Cloudflare through the proxy —
/// also our MITM/country check). Short so dead candidates are rejected fast.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(7);
/// GENEROUS timeout for the keep-warm HEALTH probe of the primary — same full-TLS
/// validation (catches a tunnel that mangles/RSTs the payload), but lenient so a
/// slow-but-working exit isn't churned.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// A primary whose health probe is slower than this counts as DEGRADING and is
/// rotated to the fresh warm backup before it fully dies. Kept ABOVE the accept
/// timeout (VALIDATE_TIMEOUT=7s) so an exit that just validated isn't judged
/// degraded on its next probe — that mismatch would churn spiky free proxies.
const DEGRADE_MS: u128 = 10_000;
/// Consecutive bad probes before rotating the primary. One blip is tolerated
/// (stability over churn); free proxies have very spiky latency.
const GRACE: u8 = 2;
/// Re-hit the free-proxy providers at most this often; between fetches we
/// re-validate the cached candidate list so a prolonged no-exit state never
/// hammers ProxyScrape/Geonode from the user's real IP (their APIs rate-limit).
const FETCH_TTL: Duration = Duration::from_secs(180);
/// Overall wall-clock cap on a validate-race (discovery or backup refill) so an
/// all-dead candidate list can't block the health loop for ~50s.
const RACE_DEADLINE: Duration = Duration::from_secs(12);
const BATCH: usize = 16;

fn allowed(country: &str) -> bool {
    ALLOWED.contains(&country)
}

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

/// One maintenance pass (~20s cadence, or on `refresh_now`): keep the PRIMARY
/// healthy and a warm BACKUP ready.
///   1. Probe the primary; on death OR degradation (after a small grace), swap to
///      the pre-validated backup INSTANTLY (make-before-break, no user gap), or
///      discover a fresh one if no backup is ready.
///   2. Top the backup up (a different exit) while the primary keeps serving, so
///      the next swap is seamless.
pub async fn maintain(shared: Arc<Shared>, m: &mut Maint) {
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    ensure_candidates(m).await;

    // 1) Primary health / rotation.
    match shared.get_exit() {
        Some(cur) => {
            let good = matches!(probe_timed(&cur.addr).await, Some(ms) if ms < DEGRADE_MS);
            if good {
                m.fails = 0;
            } else {
                m.fails += 1;
                crate::log::log(&format!(
                    "pool: primary {} ruim ({}/{})",
                    cur.addr, m.fails, GRACE
                ));
                if m.fails >= GRACE {
                    m.fails = 0;
                    if !shared.active.load(Ordering::SeqCst) {
                        return;
                    }
                    match shared.take_backup() {
                        // Make-before-break: swap to the warm, pre-validated backup
                        // (but never to the same exit that just failed).
                        Some(b) if b.addr != cur.addr => {
                            crate::log::log(&format!(
                                "pool: rotacionou {} -> reserva {} ({})",
                                cur.addr, b.ip, b.country
                            ));
                            if shared.set_exit_if_active(b.clone()) {
                                shared.set_status(format!(
                                    "saída pronta: {} · {} (troca automática)",
                                    b.country, b.ip
                                ));
                                let dir = shared.config_dir.lock().unwrap().clone();
                                crate::settings::save_last_exit(&dir, Some(b));
                            }
                        }
                        // No usable backup (empty, or it was the same dead exit):
                        // clear + rediscover (a brief gap).
                        _ => {
                            shared.clear_exit_if(&cur.addr);
                            shared.set_status("saída caiu — procurando outra…");
                            discover(shared.clone(), m).await;
                        }
                    }
                }
            }
        }
        None => discover(shared.clone(), m).await,
    }

    // 2) Keep the warm backup healthy, distinct, and topped up so the next swap is
    //    instant. Runs while the primary keeps serving.
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    // Never let the backup equal the primary — a collision makes a swap a no-op.
    if let (Some(p), Some(b)) = (shared.get_exit(), shared.get_backup()) {
        if p.addr == b.addr {
            shared.set_backup(None);
        }
    }
    // Re-validate a held backup so a stale/dead one is dropped (free proxies die
    // within minutes; an unchecked backup could be dead when promoted, defeating
    // the "no gap" swap).
    if let Some(b) = shared.get_backup() {
        if probe_timed(&b.addr).await.is_none() {
            crate::log::log(&format!("pool: reserva {} caiu", b.addr));
            shared.set_backup(None);
        }
    }
    // Refill if empty, choosing a DIFFERENT exit from the primary.
    if shared.get_backup().is_none() {
        if let Some(p) = shared.get_exit() {
            let excl = [p.addr.clone()];
            if let Some(b) = race_validate(&m.cand_addrs, &excl).await {
                if shared.get_backup().is_none()
                    && shared.get_exit().map_or(false, |p| p.addr != b.addr)
                {
                    crate::log::log(&format!("pool: reserva pronta {} ({})", b.ip, b.country));
                    shared.set_backup_if_active(b);
                }
            }
        }
    }
}

/// Refresh the cached candidate list from the providers at most every FETCH_TTL
/// (fetched_at=None counts as stale). Gated on staleness ALONE (never on
/// is_empty()), so an empty result (providers rate-limited) doesn't force an
/// immediate refetch that would feed the rate-limit loop.
async fn ensure_candidates(m: &mut Maint) {
    let stale = m.fetched_at.map_or(true, |t| t.elapsed() >= FETCH_TTL);
    if stale {
        let cands = fetch_candidates().await;
        crate::log::log(&format!("pool: {} candidatos (cache)", cands.len()));
        m.cand_addrs = cands.into_iter().map(|c| c.addr).collect();
        m.fetched_at = Some(Instant::now());
    }
}

/// Find and publish a fresh PRIMARY exit: race the cached last-working exit
/// against the free-proxy list; the first that validates wins.
async fn discover(shared: Arc<Shared>, m: &mut Maint) {
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    m.fails = 0; // the exit we're about to publish gets a fresh grace budget
    let t0 = Instant::now();
    crate::log::log("pool: procurando saída");
    shared.set_status("procurando uma saída rápida fora do Brasil…");
    let addrs = m.cand_addrs.clone();

    // Race the cached last-working exit against the free list; publish_if_first
    // keeps whichever validates first (the cached one is usually fastest — an
    // instant reconnect).
    let cached_task = m.cached.take().map(|addr| {
        let sh = shared.clone();
        tokio::spawn(async move {
            if let Some(info) = validate(&addr, VALIDATE_TIMEOUT).await {
                publish_if_first(&sh, info, "cache");
            }
        })
    });
    free_search(shared.clone(), addrs).await;
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

/// Timed health probe of the primary: full-TLS validate through the exit (catches
/// a tunnel that mangles the payload, and re-checks the country is still allowed),
/// returning its latency in ms, or None if it fails. None or a high ms => rotate.
async fn probe_timed(addr: &str) -> Option<u128> {
    let t = Instant::now();
    validate(addr, PROBE_TIMEOUT).await.map(|_| t.elapsed().as_millis())
}

/// Validate candidate addrs concurrently (up to BATCH in flight), returning the
/// FIRST that validates and whose addr is not in `exclude` — aborting the rest.
/// validate() already enforces the allowlisted-country + no-MITM checks. Used to
/// fill the warm backup without disturbing the serving primary.
async fn race_validate(addrs: &[String], exclude: &[String]) -> Option<ExitInfo> {
    use tokio::task::JoinSet;
    let deadline = Instant::now() + RACE_DEADLINE;
    let list: Vec<String> = addrs
        .iter()
        .filter(|a| !exclude.iter().any(|x| x == *a))
        .cloned()
        .collect();
    let mut idx = 0usize;
    let mut set: JoinSet<Option<ExitInfo>> = JoinSet::new();
    while idx < list.len() && set.len() < BATCH {
        let a = list[idx].clone();
        idx += 1;
        set.spawn(async move { validate(&a, VALIDATE_TIMEOUT).await });
    }
    while let Some(res) = set.join_next().await {
        if let Ok(Some(info)) = res {
            set.abort_all();
            return Some(info);
        }
        if Instant::now() >= deadline {
            set.abort_all(); // overall cap: don't block the health loop on a dead list
            break;
        }
        if idx < list.len() {
            let a = list[idx].clone();
            idx += 1;
            set.spawn(async move { validate(&a, VALIDATE_TIMEOUT).await });
        }
    }
    None
}

/// Publish an exit as the PRIMARY if we don't already hold one. First valid wins —
/// all allowed countries are equal, ranked only by reliability.
fn publish_if_first(shared: &Shared, e: ExitInfo, via: &str) {
    let mut guard = shared.exit.lock().unwrap();
    // Re-check active WHILE holding the exit lock so it serializes with deactivate
    // (which flips active=false then clears the exit under this same lock).
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    if guard.is_none() {
        *guard = Some(e.clone());
        drop(guard);
        crate::log::log(&format!("pool: saída via {via}: {} ({})", e.ip, e.country));
        shared.set_status(format!("saída pronta: {} · {} (via {})", e.country, e.ip, via));
    }
}

/// Find and publish a PRIMARY exit from the free list: the first candidate that
/// validates (allowed-country + no-MITM) wins. Excludes the current warm backup so
/// the primary and backup can never collapse to the same exit.
async fn free_search(shared: Arc<Shared>, addrs: Vec<String>) {
    let excl: Vec<String> = shared.get_backup().map(|b| vec![b.addr]).unwrap_or_default();
    if let Some(info) = race_validate(&addrs, &excl).await {
        publish_if_first(&shared, info, "free");
    }
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
    if loc.is_empty() || !allowed(&loc) {
        return None; // only exit through an allowlisted country (see ALLOWED)
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

    // Drop 4145 (SOCKS4 TLS-interception hotspot).
    cands.retain(|c| !c.addr.ends_with(":4145"));
    // Keep only candidates whose CLAIMED country is allowed or unknown (validate()
    // enforces the TRUE country); this avoids wasting validations on clearly-
    // disallowed proxies — the free lists are heavy on CN/RU/US.
    cands.retain(|c| c.country.is_empty() || allowed(&c.country));
    // Rank purely by reliability (alive, then low latency, then high uptime): any
    // allowed country is equally fine, so we just want the fastest working one.
    cands.sort_by(|a, b| {
        b.alive
            .cmp(&a.alive)
            .then(a.timeout.partial_cmp(&b.timeout).unwrap_or(std::cmp::Ordering::Equal))
            .then(b.uptime.partial_cmp(&a.uptime).unwrap_or(std::cmp::Ordering::Equal))
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
