//! Free-proxy pool: fetch, rank, validate, and keep a small set of warm SOCKS5 exits.
//!
//! Validation is a TWO-part gate, and also our MITM/credential defense. For each
//! candidate, concurrently: (1) a real TLS request to Cloudflare's trace through the
//! proxy — proves the tunnel works, the certificate is valid (a MITM is rejected),
//! and reveals the true exit IP + country (must be in ALLOWED); and (2) a SOCKS
//! CONNECT to Discord's own `gateway.discord.gg:443` — proves the exit can actually
//! reach Discord (many proxies pass Cloudflare but can't reach Discord). We accept an
//! exit ONLY if BOTH pass, and record the gateway-connect latency as its speed.
//! Port 4145 is dropped (SOCKS4 relays that intercept TLS).
//!
//! A PRIMARY exit serves the router; a small POOL of warm, pre-validated backups is
//! kept continuously pinged (gateway reachability + latency) so a dead/degraded
//! primary is swapped INSTANTLY to the fastest proven exit (make-before-break, no
//! user gap). The pool self-heals for hours: dead backups are dropped and refilled.

use crate::state::{ExitInfo, Shared, BACKUP_TARGET, TOR_PASS, TOR_USER};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;

/// Countries safe to exit through: Discord voice/Go-Live AND age-restricted (NSFW)
/// channels work there with NO government block and NO IP-triggered age/ID
/// verification (the gateway's exit IP is Discord's effective jurisdiction), as of
/// Aug 2026 — researched + adversarially verified. Excludes blocked countries
/// (CN/RU/IR/UAE/EG/TR/…) and the IP-triggered age-verification ones: GB, AU, BR, US.
/// (US was briefly added in v0.1.15 then REVERTED in v0.1.16: live validation showed
/// ~0% of free US proxies pass the full Cloudflare-TLS check — they accept a bare TCP
/// connect but can't carry a real TLS session (honeypots/HTTP-only), so they'd break
/// Discord's gateway TLS too, and the hundreds of dead US entries only crowded the
/// candidate list and starved the validator. A GOOD US exit works — but that's BYO,
/// not the free pool.) Watch-items: GR's law takes effect 2027; DK/FR have pending
/// bills.
const ALLOWED: &[&str] = &[
    "TH", "FR", "DE", "IE", "IT", "NL", "BE", "PL", "CZ", "AT", "SE", "FI", "NO", "DK",
    "PT", "RO", "GR", "CH", "CA", "MX", "AR", "CL", "CO", "UY", "NZ", "JP", "TW", "HK",
    "IN", "PH", "SG", "IL", "ZA", "NG", "KE", "UA", "MD", "GE", "AM", "LK",
];

/// Timeout for DISCOVERY / backup-refill validation (Cloudflare TLS + gateway
/// CONNECT, run concurrently). Short so dead candidates are rejected fast.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(7);
/// GENEROUS timeout for the keep-warm HEALTH probe (gateway CONNECT for a free
/// proxy; full re-validation for a Tor exit, to catch circuit drift), lenient so a
/// slow-but-working exit isn't churned.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// A primary whose health probe is slower than this counts as DEGRADING and is
/// rotated to a warm backup before it fully dies. Kept high (gateway CONNECT is
/// normally <1.5s, per live testing) so ONLY a near-dead exit rotates on slowness —
/// free proxies have very spiky latency and a lower bar churns usable exits (a fully
/// dead exit rotates regardless, since the probe returns None).
const DEGRADE_MS: u32 = 13_000;
/// Consecutive bad probes before rotating the primary. Live 3-minute monitoring
/// showed healthy free exits hit fail-streaks up to 3 before recovering, so 3
/// tolerates the real spikiness — the #1 complaint was TOO MANY rotations.
const GRACE: u8 = 3;
/// Only ever proactively swap away from a primary at least this slow (smoothed).
/// Fast primaries are never disturbed — this floor plus the 0.6× gap below keep
/// "switch to the best" upgrades rare and non-churning.
const UPGRADE_FLOOR_MS: u32 = 2500;
/// Consecutive cycles the "slow primary + much-faster backup" condition must hold
/// before we actually upgrade — so a single EWMA spike never swaps the exit.
const UPGRADE_GRACE: u8 = 2;
/// Re-hit the free-proxy providers at most this often; between fetches we
/// re-validate the cached candidate list so a prolonged no-exit state never
/// hammers ProxyScrape/Geonode from the user's real IP (their APIs rate-limit).
const FETCH_TTL: Duration = Duration::from_secs(180);
/// Overall wall-clock cap on a validate-race (discovery or backup refill) so an
/// all-dead candidate list can't block the health loop for ~50s.
const RACE_DEADLINE: Duration = Duration::from_secs(12);
/// Validations kept in flight per race. Raised 16→24 (v0.1.16): free proxies validate
/// at only ~5% live-tested, so more concurrency finds a working exit within the
/// deadline instead of leaving the user with "no exit" on a bad batch.
const BATCH: usize = 24;

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
    /// Best exits from a previous run (settings `last_exits`, reliability-first); raced
    /// IN PARALLEL on the first discovery then cleared, for an instant reconnect + a
    /// pre-warmed pool (the ones that don't win primary become warm backups).
    cached: Vec<String>,
    /// The exit addrs last written to the cache file — so we only re-write on a change.
    persisted: Vec<String>,
    /// Consecutive cycles the primary has been "slow AND a much-faster backup exists"
    /// — a proactive upgrade needs UPGRADE_GRACE of these in a row, so one latency
    /// spike never triggers a visible exit swap.
    slow: u8,
}

impl Maint {
    /// Seed with the last session's best exit addrs (from settings) for a warm start.
    pub fn new(cached: Vec<String>) -> Self {
        Maint {
            cached,
            ..Default::default()
        }
    }
}

/// One maintenance pass (~20s cadence, or on `refresh_now`): keep the PRIMARY
/// healthy and the warm BACKUP POOL full.
///   1. Probe the primary; on death OR degradation (after GRACE), swap INSTANTLY to
///      the fastest pre-validated backup (make-before-break, no user gap), or
///      discover a fresh one if the pool is empty.
///   2. Probe every backup (drop dead, update latency) and refill the pool to
///      BACKUP_TARGET with distinct exits, while the primary keeps serving.
pub async fn maintain(shared: Arc<Shared>, m: &mut Maint) {
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    ensure_candidates(m).await;
    // A pending user-refresh exclusion applies to this WHOLE pass — the new primary AND
    // the backup refill — so the refreshed-away exit isn't quietly re-pooled and then
    // promoted straight back. Consumed once here.
    let avoid = shared.take_refresh_avoid();

    // 1) Primary health / rotation.
    match shared.get_exit() {
        Some(cur) => match probe_health(&cur.addr).await {
            Some(ms) if ms < DEGRADE_MS => {
                m.fails = 0;
                shared.set_exit_latency(&cur.addr, ms); // live latency index
                maybe_upgrade(&shared, m, &cur).await; // switch to a MUCH-faster backup
            }
            _ => {
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
                    rotate_primary(&shared, m, &cur.addr, &avoid).await;
                }
            }
        },
        None => discover(shared.clone(), m, &avoid).await,
    }

    // 2) Keep the backup pool healthy and full so the next swap is instant.
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    maintain_backups(&shared, m, &avoid).await;

    // 3) Persist the top exits (best-first) for a fast warm start next launch.
    persist_top_exits(&shared, m);
}

/// Write the current best exits (primary + backups, fastest-first, excluding local Tor
/// exits which aren't reusable next launch) to the cache file so the next launch races
/// them in parallel. Skips the write when the set is unchanged, to avoid needless disk
/// churn every ~20s.
fn persist_top_exits(shared: &Arc<Shared>, m: &mut Maint) {
    let mut all: Vec<ExitInfo> = shared.get_exit().into_iter().collect();
    all.extend(shared.get_backups());
    all.retain(|e| !e.addr.starts_with("127.0.0.1:"));
    all.sort_by_key(|e| e.latency_ms);
    all.truncate(2);
    if all.is_empty() {
        return;
    }
    let addrs: Vec<String> = all.iter().map(|e| e.addr.clone()).collect();
    if addrs == m.persisted {
        return;
    }
    m.persisted = addrs;
    let dir = shared.config_dir.lock().unwrap().clone();
    crate::settings::save_last_exits(&dir, all);
}

/// The primary at `dead_addr` failed GRACE times: swap to the FASTEST warm backup
/// (instant, make-before-break), or clear + rediscover if the pool is empty.
async fn rotate_primary(shared: &Arc<Shared>, m: &mut Maint, dead_addr: &str, avoid: &[String]) {
    match shared.take_fastest_backup() {
        // Never swap to the same exit that just failed (defensive — the pool
        // excludes the primary, so this only guards a race).
        Some(b) if b.addr != dead_addr => {
            // Compare-and-set on the dead addr (install only if the exit is still the
            // dead one or empty) so we never clobber a fresh primary the router already
            // promoted; on a lost race, return b to the pool so the warm buffer isn't
            // drained. Mirrors the router + maybe_upgrade paths.
            if shared.replace_dead_exit(dead_addr, b.clone()) {
                crate::log::log(&format!(
                    "pool: rotacionou {} -> reserva {} ({}, {}ms)",
                    dead_addr, b.ip, b.country, b.latency_ms
                ));
                shared.set_status(format!(
                    "saída pronta: {} · {} (troca automática)",
                    b.country, b.ip
                ));
                // (persisted at the end of this maintain pass)
            } else {
                shared.push_backup_if_active(b); // another task already swapped in a fresh exit
            }
        }
        _ => {
            shared.clear_exit_if(dead_addr);
            shared.set_status("saída caiu — procurando outra…");
            discover(shared.clone(), m, avoid).await;
        }
    }
}

/// Proactively switch the primary to a MUCH-faster warm backup — but ONLY when the
/// current primary is genuinely slow (>= UPGRADE_FLOOR_MS) AND a backup is
/// dramatically faster (< 0.6×). This gives "best available" without churning when
/// latencies are close (a fast primary is never disturbed; the old primary, still
/// working, is kept in the pool). No-op in the common case.
async fn maybe_upgrade(shared: &Arc<Shared>, m: &mut Maint, cur: &ExitInfo) {
    let prim = match shared.get_exit() {
        Some(e) if e.addr == cur.addr => e.latency_ms,
        _ => return, // primary changed under us
    };
    if prim < UPGRADE_FLOOR_MS {
        m.slow = 0;
        return; // primary is fine — leave it alone
    }
    let best = match shared.fastest_backup_latency() {
        Some(b) => b,
        None => {
            m.slow = 0;
            return;
        }
    };
    // Require best < 0.6 × prim (integer: 5·best < 3·prim) — a wide margin so we
    // never ping-pong between similar exits.
    if (best as u64) * 5 >= (prim as u64) * 3 {
        m.slow = 0;
        return;
    }
    // The condition holds; require it to persist UPGRADE_GRACE cycles (a single
    // spike never swaps the visible exit).
    m.slow += 1;
    if m.slow < UPGRADE_GRACE {
        return;
    }
    m.slow = 0;
    if let Some(mut nb) = shared.take_fastest_backup() {
        if nb.addr == cur.addr {
            return; // paranoia: never "upgrade" to the same exit
        }
        // Re-probe the target NOW before abandoning a WORKING primary — its pooled
        // latency is up to a cycle old and it may have died since. If it's dead, drop
        // it (don't re-add) and keep the current primary.
        match gw_connect(&nb.addr, PROBE_TIMEOUT).await {
            Some(ms) => nb.latency_ms = ms,
            None => return,
        }
        // Re-confirm it's still much faster with the FRESH measurement (a spike since
        // last probe must not trade a working primary for a no-longer-faster exit).
        if (nb.latency_ms as u64) * 5 >= (prim as u64) * 3 {
            shared.push_backup_if_active(nb);
            return;
        }
        if shared.replace_exit_if(&cur.addr, nb.clone()) {
            crate::log::log(&format!(
                "pool: upgrade {} ({}ms) -> {} ({}, {}ms)",
                cur.addr, prim, nb.ip, nb.country, nb.latency_ms
            ));
            shared.set_status(format!("saída pronta: {} · {} (mais rápida)", nb.country, nb.ip));
            // (persisted at the end of this maintain pass)
            // Keep the still-working old primary warm in the pool.
            shared.push_backup_if_active(cur.clone());
        } else {
            // Primary changed under us — return the backup to the pool.
            shared.push_backup_if_active(nb);
        }
    }
}

/// Probe each warm backup (gateway reachability + latency), drop the dead, and
/// refill the pool up to BACKUP_TARGET with exits distinct from the primary and each
/// other. Runs while the primary keeps serving, so a rotation always has a proven,
/// low-latency exit ready — the app stays healthy for hours.
async fn maintain_backups(shared: &Arc<Shared>, m: &mut Maint, avoid: &[String]) {
    // Heal any transient primary==backup duplicate (a narrow cross-lock race between a
    // router promotion and a backup push can leave the current primary in the pool) so
    // a slot + probes aren't wasted on the primary itself.
    if let Some(p) = shared.get_exit() {
        shared.remove_backup(&p.addr);
    }
    // Probe existing backups: refresh latency for the survivors, drop the dead.
    for b in shared.get_backups() {
        if !shared.active.load(Ordering::SeqCst) {
            return;
        }
        match probe_health(&b.addr).await {
            Some(ms) => shared.set_backup_latency(&b.addr, ms),
            None => {
                crate::log::log(&format!("pool: reserva {} caiu", b.addr));
                shared.remove_backup(&b.addr);
            }
        }
    }
    // Refill to BACKUP_TARGET. Each pass excludes the primary + current backups so we
    // never add a duplicate; stop as soon as a pass makes no progress (candidate list
    // exhausted this cycle) so we don't spin.
    while shared.active.load(Ordering::SeqCst) && shared.backups_len() < BACKUP_TARGET {
        let before = shared.backups_len();
        let mut excl: Vec<String> = shared.get_backups().into_iter().map(|b| b.addr).collect();
        if let Some(p) = shared.get_exit() {
            excl.push(p.addr);
        }
        excl.extend_from_slice(avoid); // don't re-pool an exit the user just refreshed away
        match race_validate(&m.cand_addrs, &excl).await {
            Some(b) => {
                crate::log::log(&format!(
                    "pool: reserva pronta {} ({}, {}ms) [{}/{}]",
                    b.ip,
                    b.country,
                    b.latency_ms,
                    before + 1,
                    BACKUP_TARGET
                ));
                shared.push_backup_if_active(b);
            }
            None => break, // nothing validated this cycle — try again next pass
        }
        if shared.backups_len() <= before {
            break; // no net progress (deduped/inactive) — avoid spinning
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
async fn discover(shared: Arc<Shared>, m: &mut Maint, avoid: &[String]) {
    if !shared.active.load(Ordering::SeqCst) {
        return;
    }
    m.fails = 0; // the exit we're about to publish gets a fresh grace + upgrade budget
    m.slow = 0;
    let t0 = Instant::now();
    crate::log::log("pool: procurando saída");
    shared.set_status("procurando uma saída rápida fora do Brasil…");
    let addrs = m.cand_addrs.clone();

    // Race, concurrently: the previous session's best exits (ALL in parallel), a local
    // Tor exit (opt-in), and the free list. The first to validate wins the primary
    // (the cached ones are usually fastest — an instant reconnect); the other cached
    // exits that validate become warm backups (they worked last time, so they
    // re-validate fast and PRE-FILL the pool). Skip any the user just refreshed away,
    // and skip Tor exits (127.0.0.1) when Tor is off.
    let use_tor = shared.use_tor.load(Ordering::SeqCst);
    let cached: Vec<String> = std::mem::take(&mut m.cached)
        .into_iter()
        .filter(|addr| !avoid.contains(addr))
        .filter(|addr| use_tor || !addr.starts_with("127.0.0.1:"))
        .collect();
    let mut cached_tasks = Vec::new();
    for addr in cached {
        let sh = shared.clone();
        cached_tasks.push(tokio::spawn(async move {
            if let Some(info) = validate(&addr, VALIDATE_TIMEOUT).await {
                // Winner becomes primary; the rest pre-warm the pool.
                if !publish_if_first(&sh, info.clone(), "cache") {
                    sh.push_backup_if_active(info);
                }
            }
        }));
    }
    let tor_task = use_tor.then(|| {
        let sh = shared.clone();
        tokio::spawn(async move {
            if let Some(tor) = try_tor().await {
                publish_if_first(&sh, tor, "Tor");
            }
        })
    });
    free_search(shared.clone(), addrs, avoid.to_vec()).await;
    if let Some(t) = tor_task {
        let _ = t.await;
    }
    for t in cached_tasks {
        let _ = t.await;
    }

    // (The best exits are persisted at the end of each maintain pass, not here.)
    if shared.get_exit().is_some() {
        crate::log::log(&format!(
            "pool: saída pronta em {:.1}s",
            t0.elapsed().as_secs_f32()
        ));
    } else if shared.active.load(Ordering::SeqCst) {
        crate::log::log("pool: nenhuma saída -> gateway vai DIRETO (Go Live bloqueado)");
        // Diagnostic status: distinguish "the proxy lists themselves didn't load"
        // (network/DNS/firewall blocking the sources — the exact case where a machine
        // never finds any exit) from "lists loaded but no proxy connected".
        if m.cand_addrs.is_empty() {
            shared.set_status(
                "sem saída — nenhuma lista de proxy carregou (rede/DNS bloqueando? tente DNS 1.1.1.1)",
            );
        } else {
            shared.set_status(format!(
                "sem saída — {} proxies testados, nenhum conectou (tentando novamente…)",
                m.cand_addrs.len()
            ));
        }
    }
}

/// Health probe of an exit, returning its gateway-connect latency in ms (or None to
/// rotate). For a free proxy this is a fast SOCKS CONNECT to Discord's gateway — the
/// thing that actually matters — since a given proxy IP's country is fixed. For a
/// Tor exit (127.0.0.1) it is a FULL re-validation (Cloudflare country + gateway) so
/// a circuit that drifted to a disallowed country is caught within one cycle.
async fn probe_health(addr: &str) -> Option<u32> {
    if addr.starts_with("127.0.0.1:") {
        validate(addr, PROBE_TIMEOUT).await.map(|e| e.latency_ms)
    } else {
        gw_connect(addr, PROBE_TIMEOUT).await
    }
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

/// Publish `e` as the PRIMARY if we don't already hold one (first valid wins — all
/// allowed countries are equal, ranked only by reliability). Returns whether it became
/// the primary, so a loser can be pooled as a warm backup instead.
fn publish_if_first(shared: &Shared, e: ExitInfo, via: &str) -> bool {
    let mut guard = shared.exit.lock().unwrap();
    // Re-check active WHILE holding the exit lock so it serializes with deactivate
    // (which flips active=false then clears the exit under this same lock).
    if !shared.active.load(Ordering::SeqCst) {
        return false;
    }
    if guard.is_none() {
        *guard = Some(e.clone());
        drop(guard);
        crate::log::log(&format!("pool: saída via {via}: {} ({})", e.ip, e.country));
        shared.set_status(format!("saída pronta: {} · {} (via {})", e.country, e.ip, via));
        true
    } else {
        false
    }
}

/// Find and publish a PRIMARY exit from the free list: the first candidate that
/// validates (allowed-country + no-MITM) wins. Excludes the current warm backup so
/// the primary and backup can never collapse to the same exit.
async fn free_search(shared: Arc<Shared>, addrs: Vec<String>, extra_excl: Vec<String>) {
    let mut excl: Vec<String> = shared.get_backups().into_iter().map(|b| b.addr).collect();
    excl.extend(extra_excl);
    if let Some(info) = race_validate(&addrs, &excl).await {
        publish_if_first(&shared, info, "free");
    }
}

/// SOCKS5 proxy URL for `addr`. A local Tor proxy (127.0.0.1:*) gets fixed
/// credentials so Tor pins all our streams to one circuit (see TOR_USER); remote
/// free proxies get no auth.
fn socks_url(addr: &str) -> String {
    if addr.starts_with("127.0.0.1:") {
        format!("socks5h://{TOR_USER}:{TOR_PASS}@{addr}")
    } else {
        format!("socks5h://{addr}")
    }
}

/// Optional: if a local Tor SOCKS port is up (Tor Browser 9150 / daemon 9050) and
/// its exit lands in an ALLOWED country, use it. Only opportunistic — the user must
/// be running Tor; proxies work fine without it. A refused localhost connect fails
/// instantly, so this is nearly free when Tor isn't running.
async fn try_tor() -> Option<ExitInfo> {
    for port in [9150u16, 9050u16, 9060u16, 9052u16, 9250u16] {
        let addr = format!("127.0.0.1:{port}");
        let up = tokio::time::timeout(Duration::from_millis(400), TcpStream::connect(&addr))
            .await
            .ok()
            .and_then(|r| r.ok())
            .is_some();
        if !up {
            continue;
        }
        // validate() enforces the allowlist on the true exit country; then label it.
        if let Some(mut info) = validate(&addr, VALIDATE_TIMEOUT).await {
            info.country = format!("Tor·{}", info.country);
            return Some(info);
        }
    }
    None
}

/// Validate one SOCKS5 proxy: BOTH checks run concurrently and BOTH must pass.
///   - Cloudflare TLS trace: proves the tunnel + a valid certificate (no MITM) and
///     reveals the true exit IP/country (must be allowlisted).
///   - Gateway CONNECT: proves the exit can actually reach Discord, and times it.
/// The recorded `latency_ms` is the gateway-connect time (the Discord-relevant one),
/// used to promote the fastest backup on rotation.
async fn validate(addr: &str, timeout: Duration) -> Option<ExitInfo> {
    let (cf, gc) = tokio::join!(cf_check(addr, timeout), gw_connect(addr, timeout));
    let (ip, loc) = cf?;
    if loc.is_empty() || !allowed(&loc) {
        return None; // only exit through an allowlisted country (see ALLOWED)
    }
    let latency_ms = gc?; // must be able to reach Discord's gateway
    Some(ExitInfo {
        addr: addr.to_string(),
        ip,
        country: loc,
        latency_ms,
    })
}

/// Cloudflare trace through the proxy: returns (exit_ip, exit_country) or None. This
/// is the MITM/certificate + true-country check.
async fn cf_check(addr: &str, timeout: Duration) -> Option<(String, String)> {
    let proxy = reqwest::Proxy::all(socks_url(addr)).ok()?;
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
    Some((ip, loc))
}

/// SOCKS CONNECT to Discord's gateway through the proxy, timed. Proves the exit can
/// actually reach Discord (the check Cloudflare validation alone misses) and yields
/// the connect latency in ms. A local Tor exit uses the pinning credentials so the
/// probe rides the same circuit we validated.
async fn gw_connect(addr: &str, timeout: Duration) -> Option<u32> {
    let t = Instant::now();
    let target = ("gateway.discord.gg", 443u16);
    let connect = async {
        if addr.starts_with("127.0.0.1:") {
            Socks5Stream::connect_with_password(addr, target, TOR_USER, TOR_PASS).await
        } else {
            Socks5Stream::connect(addr, target).await
        }
    };
    match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(_)) => Some(t.elapsed().as_millis() as u32),
        _ => None,
    }
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

    // Query all providers in parallel and merge (dedup by addr) for speed + reach.
    // Live yield-testing (Aug 2026) ranked usable-exit rate proxifly ~10% > proxyscrape
    // ~7% > geonode ~1%; iplocate/iproyal/fineproxy were rejected (0.6% / unscrapable).
    let (ps, gn, px) = tokio::join!(
        fetch_proxyscrape(&client),
        fetch_geonode(&client),
        fetch_proxifly(&client)
    );
    crate::log::log(&format!(
        "pool: sources proxyscrape={} geonode={} proxifly={}",
        ps.len(),
        gn.len(),
        px.len()
    ));
    let mut cands = ps;
    cands.extend(gn);
    cands.extend(px);
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
    // Keep a wide working set: at ~5% live yield, the pool + backup refills need many
    // candidates to draw from (raised 160→320 in v0.1.16).
    cands.truncate(320);
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

/// proxifly/free-proxy-list — a plain `socks5://ip:port` text feed (no per-proxy
/// metadata). Given neutral reliability values so it interleaves with the metadata-rich
/// sources rather than sinking to the truncated tail; validate() enforces the true
/// country + Discord reachability anyway.
async fn fetch_proxifly(client: &reqwest::Client) -> Vec<Cand> {
    let url = "https://raw.githubusercontent.com/proxifly/free-proxy-list/main/proxies/protocols/socks5/data.txt";
    let txt = match client.get(url).send().await {
        Ok(r) => match r.text().await {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in txt.lines() {
        let t = line.trim().trim_start_matches("socks5://").trim();
        if t.matches(':').count() != 1 || t.split(':').any(|p| p.is_empty()) {
            continue;
        }
        out.push(Cand {
            addr: t.to_string(),
            country: String::new(), // no metadata; validate() enforces the true country
            alive: true,
            uptime: 50.0,    // neutral (unknown) so it ranks mid-pack, not buried
            timeout: 2000.0, // neutral assumed latency
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
