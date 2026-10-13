// Ollama Cloud usage tray icon (Ubuntu top panel, StatusNotifierItem via ksni).
// Lightweight: no GTK, pure D-Bus; a poller thread hits the API every 5 min.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

const POLL_SECS: u64 = 300; // 5 min (the API allows 10 req/min per endpoint)
const API: &str = "https://ollama.com/api";

// ---------------------------------------------------------------- usage model

#[derive(Debug, Clone)]
struct Usage {
    err: Option<String>,
    plan: String,
    // limits or credits (GET /api/balance); None until a fetch succeeds
    balance: Option<BalanceResp>,
    // last 7 days (GET /api/usage?range=7d)
    week: Option<UsageTotals>,
    // failures of the optional endpoints (api/me, api/usage): the balance still shows
    warnings: Vec<String>,
    // Retry-After (s) of a 429 from an optional endpoint
    retry_after: Option<u64>,
    masked_key: String,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            err: Some("carregando...".into()),
            plan: "?".into(),
            balance: None,
            week: None,
            warnings: vec![],
            retry_after: None,
            masked_key: "—".into(),
        }
    }
}

impl Usage {
    // an optional endpoint failed: keep going, but say so
    fn note(&mut self, e: ApiError) {
        self.retry_after = self.retry_after.max(e.retry_after);
        self.warnings.push(e.msg);
    }
}

// ---------------------------------------------------------------- API types

// GET /api/balance
#[derive(Deserialize, Debug, Clone)]
struct BalanceResp {
    included: Included,
    #[serde(default)]
    purchased: Purchased,
}

// one shape per pricing model
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum Included {
    // legacy plans: session/weekly limits, no dollar amounts
    Legacy { session: Limit, weekly: Limit },
    // monthly credits in dollars
    Credits {
        balance_usd: f64,
        allowance_usd: f64,
        period: Period,
    },
}

impl Included {
    // fraction already consumed (drives icon color): the tightest limit
    fn used_frac(&self) -> f64 {
        match self {
            Included::Legacy { session, weekly } => session.used_frac().max(weekly.used_frac()),
            Included::Credits {
                balance_usd,
                allowance_usd,
                ..
            } => {
                if *allowance_usd <= 0.0 {
                    0.0
                } else {
                    (credits_used(*balance_usd, *allowance_usd) / allowance_usd).clamp(0.0, 1.0)
                }
            }
        }
    }
}

fn credits_used(balance_usd: f64, allowance_usd: f64) -> f64 {
    (allowance_usd - balance_usd).max(0.0)
}

#[derive(Deserialize, Debug, Clone)]
struct Limit {
    remaining_percent: f64,
    resets_at: String,
}

impl Limit {
    fn used_frac(&self) -> f64 {
        (1.0 - self.remaining_percent / 100.0).clamp(0.0, 1.0)
    }
}

#[derive(Deserialize, Debug, Clone)]
struct Period {
    from: String,
    until: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct Purchased {
    #[serde(default)]
    balance_usd: f64,
}

// GET /api/usage: a time series; only the totals are shown
#[derive(Deserialize)]
struct UsageResp {
    totals: UsageTotals,
}

#[derive(Deserialize, Debug, Clone)]
struct UsageTotals {
    request_count: u64,
    // omitted for legacy plans
    #[serde(default)]
    usage_usd: Option<f64>,
}

#[derive(Deserialize)]
struct MeResp {
    #[serde(rename = "Plan")]
    plan: Option<String>,
}

// ---------------------------------------------------------------- tray

// directory with our custom icon for the theme lookup
fn icons_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join(".local/share/icons/hicolor/22x22/apps")
}

fn save_icons_script() -> Option<std::path::PathBuf> {
    // runtime location (installed by install.sh): ~/.local/share/ollama-usage-tray/save_icons.py
    let home = std::env::var("HOME").ok()?;
    let installed = std::path::Path::new(&home)
        .join(".local/share/ollama-usage-tray/save_icons.py");
    if installed.is_file() {
        return Some(installed);
    }
    None
}

// render assets/icon.svg -> ~/.local/share/icons/... + refresh cache (idempotent)
fn ensure_icons_installed_impl() {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let script = match save_icons_script() {
        Some(s) => s,
        None => return,
    };
    let target = std::path::Path::new(&home).join(".local/share/icons/hicolor/22x22/apps");
    // the red variant is the last one save_icons.py writes
    let dst = target.join("ollama-usage-tray-crit.png");
    if dst.is_file() {
        // refresh theme cache even if icons already exist (helps after a first install)
        let _ = std::process::Command::new("update-icon-caches")
            .arg(format!("{home}/.local/share/icons/hicolor"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        return;
    }
    let _ = std::fs::create_dir_all(&target);
    let out = std::process::Command::new("python3")
        .arg(&script)
        .arg(format!("--home={home}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match out {
        Ok(s) if s.success() => {
            let _ = std::process::Command::new("update-icon-caches")
                .arg(format!("{}/.local/share/icons/hicolor", home))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        _ => {}
    }
}

struct OllamaTray {
    usage: Arc<Mutex<Usage>>,
    refresh: Sender<()>,
    quit: Arc<AtomicBool>,
    api_key: Arc<String>,
    key_source: Arc<String>,
}

impl ksni::Tray for OllamaTray {
    fn id(&self) -> String {
        "ollama-usage-tray".into()
    }

    fn title(&self) -> String {
        let u = self.usage.lock().unwrap();
        match &u.balance {
            Some(b) => title_text(&b.included),
            None => "…".into(),
        }
    }

    fn icon_name(&self) -> String {
        let u = self.usage.lock().unwrap();
        let Some(b) = &u.balance else {
            return "network-offline".into();
        };
        let name = icon_variant(b.included.used_frac());
        // installs older than the colored variants only have the blue icon
        if icons_dir().join(format!("{name}.png")).is_file() {
            name.into()
        } else {
            "ollama-usage-tray".into()
        }
    }

    fn icon_theme_path(&self) -> String {
        icons_dir().display().to_string()
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let u = self.usage.lock().unwrap();
        let mut tt = ksni::ToolTip::default();
        tt.title = "Ollama Cloud".into();
        if u.balance.is_none() {
            tt.description = format!(
                "Sem dados{}\nAguardando o próximo ciclo de atualização.",
                u.err
                    .as_ref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
            return tt;
        }
        tt.description = format_tooltip(&u, unix_now());
        tt
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let info: Vec<String> = {
            let u = self.usage.lock().unwrap();
            if u.balance.is_none() {
                vec![format!("Sem dados ({})", u.err.as_deref().unwrap_or("…"))]
            } else {
                detail_lines(&u, unix_now())
            }
        };
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: "Atualizar agora".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(move |t: &mut Self| {
                    let _ = t.refresh.send(());
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        items.extend(info.into_iter().map(|label| {
            StandardItem {
                label,
                enabled: false,
                ..Default::default()
            }
            .into()
        }));
        items.extend(vec![
            MenuItem::Separator,
            StandardItem {
                label: "Abrir ollama.com/settings".into(),
                icon_name: "web-browser".into(),
                activate: Box::new(|_t| {
                    let _ = std::process::Command::new("xdg-open")
                        .arg("https://ollama.com/settings")
                        .spawn();
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("API key: {}", mask_key(&self.api_key)),
                enabled: false,
                ..Default::default()
            }
            .into(),
            {
                let api_key = Arc::clone(&self.api_key);
                let masked = mask_key(&self.api_key);
                StandardItem {
                    label: format!("Copiar API key ({masked})"),
                    icon_name: "edit-copy".into(),
                    activate: Box::new(move |_t| {
                        let mut child = std::process::Command::new("xclip")
                            .args(["-selection", "clipboard"])
                            .stdin(std::process::Stdio::piped())
                            .spawn();
                        match child.as_mut() {
                            Ok(c) => {
                                use std::io::Write;
                                if let Some(stdin) = c.stdin.as_mut() {
                                    let _ = stdin.write_all(api_key.as_bytes());
                                    let _ = stdin.write_all(b"\n");
                                }
                                let _ = c.wait();
                            }
                            Err(e) => {
                                let _ = std::process::Command::new("zenity")
                                    .args(["--error", "--title=Ollama Usage Tray", "--text"])
                                    .arg(format!("xclip falhou: {e}\nInstale com: sudo apt-get install -y xclip"))
                                    .spawn();
                            }
                        }
                    }),
                    ..Default::default()
                }
            }
            .into(),
            {
                let key_source = Arc::clone(&self.key_source);
                let api_key = Arc::clone(&self.api_key);
                StandardItem {
                    label: "Abrir pasta da API key".into(),
                    icon_name: "folder-open".into(),
                    activate: Box::new(move |_t| {
                        // source is "env VAR", "arquivo /path" or "opencode /path"
                        let path = if key_source.starts_with("arquivo ") {
                            Some(key_source["arquivo ".len()..].to_string())
                        } else if key_source.starts_with("opencode ") {
                            Some(key_source["opencode ".len()..].to_string())
                        } else {
                            None // env var: no folder to open
                        };
                        match path {
                            Some(p) => {
                                let _ = std::process::Command::new("nautilus")
                                    .arg(&p)
                                    .spawn();
                            }
                            None => {
                                let _ = std::process::Command::new("zenity")
                                    .args(["--info", "--title=Ollama Usage Tray", "--text"])
                                    .arg(format!(
                                        "A key vem da variável de ambiente OLLAMA_USAGE_TRAY_KEY ({}), não de um arquivo.",
                                        mask_key(&api_key)
                                    ))
                                    .spawn();
                            }
                        }
                    }),
                    ..Default::default()
                }
            }
            .into(),
            StandardItem {
                label: "Sair".into(),
                icon_name: "application-exit".into(),
                activate: {
                    let quit = Arc::clone(&self.quit);
                    Box::new(move |_t| quit.store(true, Ordering::SeqCst))
                },
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

// "S43% · W10%" (legacy session/weekly limits) or "$28/$100" (monthly credits)
fn title_text(inc: &Included) -> String {
    match inc {
        Included::Legacy { session, weekly } => format!(
            "S{:.0}% · W{:.0}%",
            session.used_frac() * 100.0,
            weekly.used_frac() * 100.0
        ),
        Included::Credits {
            balance_usd,
            allowance_usd,
            ..
        } => format!(
            "${:.0}/${:.0}",
            credits_used(*balance_usd, *allowance_usd),
            allowance_usd
        ),
    }
}

// ring color of the icon: blue, orange >= 70%, red >= 90% (rendered by save_icons.py)
fn icon_variant(frac: f64) -> &'static str {
    if frac >= 0.9 {
        "ollama-usage-tray-crit"
    } else if frac >= 0.7 {
        "ollama-usage-tray-warn"
    } else {
        "ollama-usage-tray"
    }
}

// one line per fact, shared by the menu, the tooltip and `key-info`
fn detail_lines(u: &Usage, now: i64) -> Vec<String> {
    let mut lines = vec![format!("Plano: {}", u.plan)];
    if let Some(b) = &u.balance {
        match &b.included {
            Included::Legacy { session, weekly } => {
                lines.push(limit_line("Sessão", session, now));
                lines.push(limit_line("Semana", weekly, now));
            }
            Included::Credits {
                balance_usd,
                allowance_usd,
                period,
            } => {
                lines.push(format!(
                    "Créditos do plano: ${:.2} de ${:.2} usados ({:.1}%) · ${:.2} disponível",
                    credits_used(*balance_usd, *allowance_usd),
                    allowance_usd,
                    b.included.used_frac() * 100.0,
                    balance_usd
                ));
                lines.push(format!(
                    "Período: {} a {} · {}",
                    date_label(&period.from),
                    date_label(&period.until),
                    reset_label(&period.until, now)
                ));
            }
        }
        lines.push(format!(
            "Créditos comprados: ${:.2}",
            b.purchased.balance_usd
        ));
    }
    if let Some(w) = &u.week {
        lines.push(match w.usage_usd {
            Some(usd) => format!("Requests (7 dias): {} · ${usd:.2}", w.request_count),
            None => format!("Requests (7 dias): {}", w.request_count),
        });
    }
    lines
}

fn limit_line(name: &str, l: &Limit, now: i64) -> String {
    format!(
        "{name}: {:.1}% usado · {}",
        l.used_frac() * 100.0,
        reset_label(&l.resets_at, now)
    )
}

fn format_tooltip(u: &Usage, now: i64) -> String {
    let mut lines = detail_lines(u, now);
    lines.push(format!("API key: {}", u.masked_key));
    lines.extend(u.warnings.iter().map(|w| format!("Aviso: {w}")));
    lines.join("\n")
}

// ---------------------------------------------------------------- reset date

// "renova em 1h21 (16:00)", "renova em 5d 6h (19/10 21:00)" — local time
fn reset_label(ts: &str, now: i64) -> String {
    let Some(at) = parse_rfc3339(ts) else {
        return format!("renova em {ts}");
    };
    let t = local_time(at);
    let hm = format!("{:02}:{:02}", t.hour, t.minute);
    let secs = at - now;
    if secs <= 0 {
        format!("renovou ({hm})")
    } else if secs < 86_400 {
        format!("renova em {} ({hm})", until_label(secs))
    } else {
        format!(
            "renova em {} ({:02}/{:02} {hm})",
            until_label(secs),
            t.day,
            t.month
        )
    }
}

// "15/10", local time
fn date_label(ts: &str) -> String {
    match parse_rfc3339(ts) {
        Some(at) => {
            let t = local_time(at);
            format!("{:02}/{:02}", t.day, t.month)
        }
        None => ts.to_string(),
    }
}

// time left until a reset: "21 min", "1h21", "5d 6h"
fn until_label(secs: i64) -> String {
    // round up, so that a pending reset never reads as "0 min"
    let mins = (secs + 59) / 60;
    if mins < 60 {
        format!("{mins} min")
    } else if mins < 24 * 60 {
        format!("{}h{:02}", mins / 60, mins % 60)
    } else {
        let hours = mins / 60;
        format!("{}d {}h", hours / 24, hours % 24)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// "2026-10-13T19:00:00Z" (also fractional seconds and "+hh:mm" offsets) -> unix seconds
fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = s.get(19..)?;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        rest = &frac[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (oh, om) = rest[1..].split_once(':')?;
            if oh.len() != 2 || om.len() != 2 {
                return None;
            }
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset)
}

// epoch days from a civil date (Howard Hinnant's algorithm)
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// civil date from epoch days (Howard Hinnant's algorithm)
fn epoch_to_ymd(secs: i64) -> (i64, u32, u32) {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

struct LocalTime {
    day: u32,
    month: u32,
    hour: u32,
    minute: u32,
}

// the API speaks UTC; show resets in the user's timezone
fn local_time(secs: i64) -> LocalTime {
    let t = secs as libc::time_t;
    // SAFETY: tm is plain old data; localtime_r only writes into the struct we own
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        // no timezone info: fall back to UTC
        let (_, month, day) = epoch_to_ymd(secs);
        let s = secs.rem_euclid(86_400);
        return LocalTime {
            day,
            month,
            hour: (s / 3600) as u32,
            minute: (s % 3600 / 60) as u32,
        };
    }
    LocalTime {
        day: tm.tm_mday as u32,
        month: tm.tm_mon as u32 + 1,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
    }
}

// ---------------------------------------------------------------- networking

fn key_file_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".config/ollama-usage-tray.key")
}

// precedence: env OLLAMA_USAGE_TRAY_KEY > ~/.config/ollama-usage-tray.key
//             > opencode auth.json (ollama-cloud.key)
// returns (key, source description)
fn read_api_key() -> Result<(String, String), String> {
    if let Some(k) = std::env::var_os("OLLAMA_USAGE_TRAY_KEY") {
        let k = k.to_string_lossy().trim().to_string();
        if !k.is_empty() {
            return Ok((k, "env OLLAMA_USAGE_TRAY_KEY".into()));
        }
    }
    let kf = key_file_path();
    if let Ok(data) = std::fs::read_to_string(&kf) {
        let k = data.trim().to_string();
        if !k.is_empty() {
            let src = format!("arquivo {}", kf.display());
            return Ok((k, src));
        }
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or("HOME não definido")?;
    let path = home.join(".local/share/opencode/auth.json");
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("ler {}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("parse auth.json: {e}"))?;
    let key = v
        .pointer("/ollama-cloud/key")
        .and_then(|k| k.as_str())
        .ok_or("chave ollama-cloud não encontrada")?
        .to_string();
    if key.is_empty() {
        return Err("chave ollama-cloud vazia".into());
    }
    Ok((key, format!("opencode {}", path.display())))
}

#[derive(Debug)]
struct ApiError {
    msg: String,
    // seconds from the Retry-After header of a 429
    retry_after: Option<u64>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

// decodes a JSON response; failures carry the API's own `error` message
fn read_json<T: serde::de::DeserializeOwned>(
    endpoint: &str,
    res: Result<ureq::Response, ureq::Error>,
) -> Result<T, ApiError> {
    let resp = res.map_err(|e| api_error(endpoint, e))?;
    resp.into_json().map_err(|e| ApiError {
        msg: format!("{endpoint}: resposta inesperada: {e}"),
        retry_after: None,
    })
}

fn api_error(endpoint: &str, e: ureq::Error) -> ApiError {
    match e {
        ureq::Error::Status(code, resp) => {
            let retry_after = if code == 429 {
                resp.header("Retry-After")
                    .and_then(|v| v.trim().parse::<u64>().ok())
            } else {
                None
            };
            let status = resp.status_text().to_string();
            let detail = resp
                .into_string()
                .ok()
                .and_then(|body| error_field(&body))
                .unwrap_or(status);
            let mut msg = format!("{endpoint}: HTTP {code}: {detail}");
            if let Some(s) = retry_after {
                msg.push_str(&format!(" (nova tentativa em {s} s)"));
            }
            ApiError { msg, retry_after }
        }
        ureq::Error::Transport(t) => ApiError {
            msg: format!("{endpoint}: {t}"),
            retry_after: None,
        },
    }
}

// Ollama's JSON error format: {"error": "..."}
fn error_field(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")?.as_str().map(str::to_string)
}

fn fetch_usage(key: &str, _key_source: &str) -> Result<Usage, ApiError> {
    // native-tls: usa o keystore do sistema (obrigatório em rede com CA corporativa, ex.: Netskope)
    let tls = native_tls::TlsConnector::new().map_err(|e| ApiError {
        msg: format!("tls: {e}"),
        retry_after: None,
    })?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(Duration::from_secs(15))
        .build();
    let auth = format!("Bearer {key}");

    // limits or credits (GET /api/balance): the essential data — without it there is nothing to show
    let balance: BalanceResp = read_json(
        "api/balance",
        agent
            .get(&format!("{API}/balance"))
            .set("Authorization", &auth)
            .call(),
    )?;
    let mut usage = Usage {
        err: None,
        balance: Some(balance),
        masked_key: mask_key(key),
        ..Default::default()
    };

    // plan name (POST /api/me)
    match read_json::<MeResp>(
        "api/me",
        agent
            .post(&format!("{API}/me"))
            .set("Authorization", &auth)
            .set("Content-Type", "application/json")
            .send_string("{}"),
    ) {
        Ok(me) => usage.plan = me.plan.unwrap_or_else(|| "?".into()),
        Err(e) => usage.note(e),
    }

    // request totals of the last 7 days (GET /api/usage); per-model counts are no longer available
    match read_json::<UsageResp>(
        "api/usage",
        agent
            .get(&format!("{API}/usage"))
            .query("range", "7d")
            .set("Authorization", &auth)
            .call(),
    ) {
        Ok(r) => usage.week = Some(r.totals),
        Err(e) => usage.note(e),
    }

    Ok(usage)
}

// ---------------------------------------------------------------- main

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        std::process::exit(run_cli(&args[1..]));
    }
    run_tray();
}

// ---------------------------------------------------------------- CLI (key management)

fn usage_cli() -> ! {
    eprintln!(
        "uso:\n  \
         ollama-usage-tray                    inicia o tray\n  \
         ollama-usage-tray set-key <API_KEY>   define a API key (salva em ~/.config/ollama-usage-tray.key)\n  \
         ollama-usage-tray set-key             lê a key do stdin (echo $KEY | ollama-usage-tray set-key)\n  \
         ollama-usage-tray show-key            mostra a key em uso (com origem)\n  \
         ollama-usage-tray clear-key           remove o override; volta a usar a key do opencode\n  \
         ollama-usage-tray key-info            valida a key e mostra plano/uso sem abrir o tray"
    );
    std::process::exit(2);
}

fn run_cli(args: &[String]) -> i32 {
    let cmd = args[0].as_str();
    match cmd {
        "set-key" => {
            let key = if args.len() >= 2 {
                args[1].trim().to_string()
            } else {
                use std::io::Read;
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                buf.trim().to_string()
            };
            if key.is_empty() {
                eprintln!("erro: key vazia");
                return 1;
            }
            // validate before saving
            match fetch_usage(&key, "validação") {
                Ok(u) => println!("key válida — plano: {}", u.plan),
                Err(e) => {
                    eprintln!("aviso: key não validada ({e}); salvando mesmo assim");
                }
            }
            let kf = key_file_path();
            if let Some(dir) = kf.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::write(&kf, format!("{key}\n")) {
                Ok(_) => {
                    let _ = std::fs::set_permissions(&kf, {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::Permissions::from_mode(0o600)
                    });
                    println!("salvo em {}", kf.display());
                    println!("reinicie o tray para aplicar: pkill -f 'bin/ollama-usage-tray' && ~/.local/bin/ollama-usage-tray &");
                    0
                }
                Err(e) => {
                    eprintln!("erro ao salvar: {e}");
                    1
                }
            }
        }
        "show-key" => match read_api_key() {
            Ok((k, src)) => {
                let masked = mask_key(&k);
                println!("{masked}");
                println!("origem: {src}");
                0
            }
            Err(e) => {
                eprintln!("erro: {e}");
                1
            }
        },
        "clear-key" => {
            let kf = key_file_path();
            match std::fs::remove_file(&kf) {
                Ok(_) => {
                    println!("override removido ({})", kf.display());
                    println!("voltando a usar a key do opencode");
                    0
                }
                Err(_) => {
                    println!("nenhum override para remover");
                    0
                }
            }
        }
        "key-info" => match read_api_key() {
            Ok((k, src)) => {
                println!("origem: {src}");
                match fetch_usage(&k, &src) {
                    Ok(u) => {
                        for line in detail_lines(&u, unix_now()) {
                            println!("{line}");
                        }
                        for w in &u.warnings {
                            eprintln!("aviso: {w}");
                        }
                        0
                    }
                    Err(e) => {
                        eprintln!("erro ao consultar uso: {e}");
                        1
                    }
                }
            }
            Err(e) => {
                eprintln!("erro: {e}");
                1
            }
        },
        "-h" | "--help" | "help" => usage_cli(),
        _ => {
            eprintln!("comando desconhecido: {cmd}");
            usage_cli();
        }
    }
}

fn mask_key(k: &str) -> String {
    let t = k.trim();
    if t.len() <= 8 {
        format!("{}••••", &t[..2.min(t.len())])
    } else {
        format!("{}••••{}", &t[..4], &t[t.len() - 4..])
    }
}

// ---------------------------------------------------------------- tray run

fn run_tray() {
    // ---- single instance (fd lock on a lockfile; released on process exit) ----
    let lock_path = std::env::temp_dir().join("ollama-usage-tray.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&lock_path)
        .unwrap_or_else(|e| panic!("abrir lock {lock_path_display}: {e}", lock_path_display = lock_path.display()));
    let mut lock = fd_lock::RwLock::new(lock_file);
    // hold the write guard for the whole process lifetime -> single instance
    let _guard = match lock.try_write() {
        Ok(guard) => guard,
        Err(_) => {
            eprintln!("ollama-usage-tray: outra instância já está rodando");
            std::process::exit(0);
        }
    };

    // ---- icons (install once so the tray can use a custom SVG icon) ----
    ensure_icons_installed_impl();

    // ---- api key ----
    let (key, key_source) = read_api_key().unwrap_or_else(|e| {
        eprintln!("erro: {e}");
        std::process::exit(1);
    });

    // ---- shared state ----
    let usage = Arc::new(Mutex::new(Usage::default()));
    let quit = Arc::new(AtomicBool::new(false));

    // ---- refresh channel: menu "Atualizar" -> poller thread ----
    let (tx, rx) = mpsc::channel::<()>();

    // ---- poller thread: fetch now, then every 5 min (or on demand) ----
    {
        let usage = Arc::clone(&usage);
        let key = key.clone();
        let key_source = key_source.clone();
        let quit = Arc::clone(&quit);
        std::thread::spawn(move || poller(usage, key, key_source, rx, quit));
    }

    // ---- tray service (blocks in its own thread; we park here) ----
    let tray = OllamaTray {
        usage: Arc::clone(&usage),
        refresh: tx,
        quit: Arc::clone(&quit),
        api_key: Arc::new(key),
        key_source: Arc::new(key_source),
    };
    let handle = ksni::blocking::TrayMethods::spawn(tray)
        .unwrap_or_else(|e| panic!("spawn tray: {e}"));

    // main thread: sleep in small steps, checking quit flag; refresh tray title/icon
    let mut last_update = Instant::now();
    loop {
        if quit.load(Ordering::SeqCst) {
            break;
        }
        // periodically re-read usage into the tray (title/icon/tooltip)
        if last_update.elapsed() >= Duration::from_secs(5) {
            last_update = Instant::now();
            let _ = handle.update(|_t| {});
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = handle.shutdown().wait();
    println!("ollama-usage-tray: tchau");
}

fn poller(
    usage: Arc<Mutex<Usage>>,
    key: String,
    key_source: String,
    rx: Receiver<()>,
    quit: Arc<AtomicBool>,
) {
    let interval = Duration::from_secs(POLL_SECS);
    let mut next_fetch = Instant::now(); // fetch immediately
    // after a 429 nothing is sent before this, not even "Atualizar agora"
    let mut not_before = Instant::now();
    loop {
        if quit.load(Ordering::SeqCst) {
            return;
        }
        if Instant::now() >= next_fetch {
            let fresh = fetch_usage(&key, &key_source);
            let now = Instant::now();
            next_fetch = now + interval;
            let retry_after = match &fresh {
                Ok(nu) => nu.retry_after,
                Err(e) => e.retry_after,
            };
            if let Some(s) = retry_after {
                not_before = now + Duration::from_secs(s.max(5));
                if fresh.is_err() {
                    // no data to show: retry as soon as the API allows
                    next_fetch = not_before;
                }
            }
            let mut u = usage.lock().unwrap();
            match fresh {
                Ok(nu) => *u = nu,
                Err(e) => {
                    u.balance = None;
                    u.err = Some(e.msg);
                }
            }
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(_) => next_fetch = Instant::now().max(not_before), // manual refresh
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // GET /api/balance, legacy plan (observed response)
    const BALANCE_LEGACY: &str = r#"{
      "included": {
        "session": { "remaining_percent": 57.32, "resets_at": "2026-10-13T19:00:00Z" },
        "weekly":  { "remaining_percent": 90.08, "resets_at": "2026-10-19T00:00:00Z" }
      },
      "purchased": { "balance_usd": 0 }
    }"#;

    // GET /api/balance, monthly credits (docs.ollama.com/api/balance)
    const BALANCE_CREDITS: &str = r#"{
      "included": {
        "balance_usd": 72.5,
        "allowance_usd": 100,
        "period": { "from": "2026-09-15T09:30:00Z", "until": "2026-10-15T09:30:00Z" }
      },
      "purchased": { "balance_usd": 25 }
    }"#;

    // GET /api/usage?range=7d, legacy plan: no usage_usd nor tokens
    const USAGE_LEGACY: &str = r#"{
      "range": "7d", "scope": "self", "granularity": "day",
      "from": "2026-10-06T00:00:00Z", "until": "2026-10-13T17:38:36Z",
      "totals": { "request_count": 2922 },
      "buckets": [
        { "from": "2026-10-06T00:00:00Z", "until": "2026-10-07T00:00:00Z", "request_count": 578 },
        { "from": "2026-10-13T00:00:00Z", "until": "2026-10-13T17:38:36Z", "partial": true, "request_count": 388 }
      ]
    }"#;

    // GET /api/usage?range=24h, new plan (docs.ollama.com/api/cloud-usage)
    const USAGE_CREDITS: &str = r#"{
      "range": "24h", "scope": "self", "granularity": "hour",
      "from": "2026-09-30T02:00:00Z", "until": "2026-10-01T02:30:00Z",
      "totals": {
        "request_count": 15, "usage_usd": 0.01718,
        "input_tokens": 106000, "cached_input_tokens": 46000, "output_tokens": 13600
      },
      "buckets": [
        { "from": "2026-10-01T02:00:00Z", "until": "2026-10-01T02:30:00Z", "partial": true,
          "request_count": 3, "usage_usd": 0.00318, "input_tokens": 18000,
          "cached_input_tokens": 6000, "output_tokens": 2400 }
      ]
    }"#;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn usage_with(balance: &str, week: Option<&str>) -> Usage {
        Usage {
            err: None,
            plan: "pro".into(),
            balance: Some(serde_json::from_str(balance).unwrap()),
            week: week.map(|w| serde_json::from_str::<UsageResp>(w).unwrap().totals),
            ..Default::default()
        }
    }

    #[test]
    fn balance_legacy() {
        let b: BalanceResp = serde_json::from_str(BALANCE_LEGACY).unwrap();
        let Included::Legacy { session, weekly } = &b.included else {
            panic!("expected legacy limits, got {:?}", b.included);
        };
        assert!(approx(session.used_frac(), 1.0 - 0.5732));
        assert!(approx(weekly.used_frac(), 1.0 - 0.9008));
        assert_eq!(parse_rfc3339(&session.resets_at), Some(1_791_918_000));
        assert_eq!(parse_rfc3339(&weekly.resets_at), Some(1_792_368_000));
        // the icon follows the tightest limit
        assert!(approx(b.included.used_frac(), session.used_frac()));
        assert_eq!(b.purchased.balance_usd, 0.0);
        assert_eq!(title_text(&b.included), "S43% · W10%");
    }

    #[test]
    fn balance_credits() {
        let b: BalanceResp = serde_json::from_str(BALANCE_CREDITS).unwrap();
        let Included::Credits {
            balance_usd,
            allowance_usd,
            period,
        } = &b.included
        else {
            panic!("expected monthly credits, got {:?}", b.included);
        };
        assert_eq!((*balance_usd, *allowance_usd), (72.5, 100.0));
        assert_eq!(parse_rfc3339(&period.until), Some(1_792_056_600));
        assert!(approx(b.included.used_frac(), 0.275));
        assert_eq!(b.purchased.balance_usd, 25.0);
        assert_eq!(title_text(&b.included), "$28/$100");
    }

    #[test]
    fn balance_without_purchased() {
        let b: BalanceResp = serde_json::from_str(
            r#"{"included": {"balance_usd": 0, "allowance_usd": 0, "period": {"from": "x", "until": "y"}}}"#,
        )
        .unwrap();
        assert_eq!(b.purchased.balance_usd, 0.0);
        // no allowance: nothing to divide by
        assert_eq!(b.included.used_frac(), 0.0);
    }

    #[test]
    fn balance_unknown_shape_is_an_error() {
        assert!(serde_json::from_str::<BalanceResp>(r#"{"included": {}, "purchased": {"balance_usd": 0}}"#).is_err());
    }

    #[test]
    fn usage_without_usd() {
        let u: UsageResp = serde_json::from_str(USAGE_LEGACY).unwrap();
        assert_eq!(u.totals.request_count, 2922);
        assert_eq!(u.totals.usage_usd, None);
    }

    #[test]
    fn usage_with_usd() {
        let u: UsageResp = serde_json::from_str(USAGE_CREDITS).unwrap();
        assert_eq!(u.totals.request_count, 15);
        assert_eq!(u.totals.usage_usd, Some(0.01718));
    }

    #[test]
    fn detail_lines_legacy() {
        let u = usage_with(BALANCE_LEGACY, Some(USAGE_LEGACY));
        // 2h before the session resets
        let lines = detail_lines(&u, 1_791_918_000 - 2 * 3600);
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines[0], "Plano: pro");
        assert!(lines[1].starts_with("Sessão: 42.7% usado · renova em 2h00 ("), "{}", lines[1]);
        assert!(lines[2].starts_with("Semana: 9.9% usado · renova em 5d 7h ("), "{}", lines[2]);
        assert_eq!(lines[3], "Créditos comprados: $0.00");
        assert_eq!(lines[4], "Requests (7 dias): 2922");
    }

    #[test]
    fn detail_lines_credits() {
        let u = usage_with(BALANCE_CREDITS, Some(USAGE_CREDITS));
        let lines = detail_lines(&u, 1_792_056_600 - 3600);
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(
            lines[1],
            "Créditos do plano: $27.50 de $100.00 usados (27.5%) · $72.50 disponível"
        );
        assert!(lines[2].starts_with("Período: "), "{}", lines[2]);
        assert!(lines[2].contains(" · renova em 1h00 ("), "{}", lines[2]);
        assert_eq!(lines[3], "Créditos comprados: $25.00");
        assert_eq!(lines[4], "Requests (7 dias): 15 · $0.02");
    }

    #[test]
    fn detail_lines_without_optional_endpoints() {
        // api/me and api/usage failed: the balance still shows
        let mut u = usage_with(BALANCE_LEGACY, None);
        u.plan = "?".into();
        u.note(ApiError {
            msg: "api/usage: HTTP 503: unavailable".into(),
            retry_after: None,
        });
        let lines = detail_lines(&u, 0);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(format_tooltip(&u, 0).ends_with("\nAviso: api/usage: HTTP 503: unavailable"));
    }

    #[test]
    fn note_keeps_the_longest_retry_after() {
        let mut u = Usage::default();
        for retry_after in [Some(30), None, Some(12)] {
            u.note(ApiError {
                msg: "x".into(),
                retry_after,
            });
        }
        assert_eq!(u.retry_after, Some(30));
        assert_eq!(u.warnings.len(), 3);
    }

    #[test]
    fn icon_variants() {
        assert_eq!(icon_variant(0.0), "ollama-usage-tray");
        assert_eq!(icon_variant(0.69), "ollama-usage-tray");
        assert_eq!(icon_variant(0.7), "ollama-usage-tray-warn");
        assert_eq!(icon_variant(0.9), "ollama-usage-tray-crit");
        assert_eq!(icon_variant(1.0), "ollama-usage-tray-crit");
    }

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2024-02-29T12:34:56Z"), Some(1_709_210_096));
        assert_eq!(parse_rfc3339("2024-02-29T12:34:56.789Z"), Some(1_709_210_096));
        assert_eq!(parse_rfc3339("2024-02-29T09:34:56-03:00"), Some(1_709_210_096));
        assert_eq!(parse_rfc3339("2024-02-29T13:34:56+01:00"), Some(1_709_210_096));
        assert_eq!(parse_rfc3339("2024-02-29"), None);
        assert_eq!(parse_rfc3339("2024-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("2024-02-29T12:34:56"), None);
        assert_eq!(parse_rfc3339("2024-02-29T12:34:56+0300"), None);
        assert_eq!(parse_rfc3339("+024-02-29T12:34:56Z"), None);
    }

    #[test]
    fn civil_round_trip() {
        for secs in [0, 951_782_400, 1_709_210_096, 1_792_368_000] {
            let (y, m, d) = epoch_to_ymd(secs);
            assert_eq!(
                days_from_civil(y, m as i64, d as i64),
                secs.div_euclid(86_400)
            );
        }
    }

    #[test]
    fn until_labels() {
        assert_eq!(until_label(1), "1 min");
        assert_eq!(until_label(59 * 60), "59 min");
        assert_eq!(until_label(60 * 60), "1h00");
        assert_eq!(until_label(81 * 60), "1h21");
        assert_eq!(until_label(5 * 86_400 + 6 * 3600), "5d 6h");
    }

    #[test]
    fn reset_labels() {
        assert!(reset_label("2026-10-13T19:00:00Z", 1_791_918_000).starts_with("renovou ("));
        assert!(reset_label("2026-10-13T19:00:00Z", 1_791_918_000 - 60).starts_with("renova em 1 min ("));
        assert_eq!(reset_label("amanhã", 0), "renova em amanhã");
    }

    #[test]
    fn error_messages() {
        assert_eq!(
            error_field(r#"{"error": "unsupported or repeated parameter: foo"}"#).as_deref(),
            Some("unsupported or repeated parameter: foo")
        );
        assert_eq!(error_field("<html>bad gateway</html>"), None);
        assert_eq!(error_field(r#"{"message": "x"}"#), None);
    }
}
