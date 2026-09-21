// Ollama Cloud usage tray icon (Ubuntu top panel, StatusNotifierItem via ksni).
// Lightweight: no GTK, pure D-Bus; a poller thread hits the API every 5 min.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

const POLL_SECS: u64 = 300; // 5 min
const RESET_DAY: u32 = 26; // plan started on the 26th (account CreatedAt 2026-05-26)

// ---------------------------------------------------------------- usage model

#[derive(Debug, Clone)]
struct Usage {
    ok: bool,
    err: Option<String>,
    plan: String,
    plan_total: f64,
    weekly_frac: f64,
    session_frac: f64,
    requests: Vec<(String, u64)>,
    masked_key: String,
    // new pricing model: monthly usage in dollars (old model uses session/weekly fractions)
    monthly_cost: Option<f64>,
    has_session_weekly: bool,
    monthly_period: Option<String>,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            ok: false,
            err: Some("carregando...".into()),
            plan: "?".into(),
            plan_total: 0.0,
            weekly_frac: 0.0,
            session_frac: 0.0,
            requests: vec![],
            masked_key: "—".into(),
            monthly_cost: None,
            has_session_weekly: false,
            monthly_period: None,
        }
    }
}

impl Usage {
    fn weekly_used(&self) -> f64 {
        self.weekly_frac * self.plan_total
    }
    fn weekly_left(&self) -> f64 {
        (self.plan_total - self.weekly_used()).max(0.0)
    }
    // dollar amount actually used this cycle (new monthly model: activity.cost; old model: weekly_frac * plan_total)
    fn used_amount(&self) -> f64 {
        self.monthly_cost.unwrap_or_else(|| self.weekly_used())
    }
    fn remaining(&self) -> f64 {
        (self.plan_total - self.used_amount()).max(0.0)
    }
    // fraction of the monthly plan already consumed (drives title + icon color)
    fn plan_frac(&self) -> f64 {
        if self.plan_total <= 0.0 {
            0.0
        } else {
            (self.used_amount() / self.plan_total).clamp(0.0, 1.0)
        }
    }
    // true if the account still uses the old session/weekly limits
    fn legacy_limits(&self) -> bool {
        self.has_session_weekly && self.monthly_cost.is_none()
    }
}

// ---------------------------------------------------------------- API types

#[derive(Deserialize)]
struct UsageResp {
    limits: Option<Limits>,
    #[serde(default)]
    activity: Option<Activity>,
}

#[derive(Deserialize)]
struct Activity {
    #[serde(default)]
    cost: Option<String>,
    #[serde(default)]
    period: Option<Period>,
}

#[derive(Deserialize)]
struct Period {
    #[serde(default, rename = "type")]
    typ: Option<String>,
    #[serde(default, rename = "starting_at")]
    #[allow(dead_code)]
    starting_at: Option<String>,
    #[serde(default, rename = "ending_at")]
    #[allow(dead_code)]
    ending_at: Option<String>,
}

#[derive(Deserialize)]
struct Limits {
    session: Option<Session>,
    weekly: Option<Weekly>,
}

#[derive(Deserialize)]
struct Session {
    usage: Option<f64>,
    #[serde(default)]
    models: Option<Vec<ModelCount>>,
}

#[derive(Deserialize)]
struct Weekly {
    usage: Option<f64>,
    #[serde(default)]
    models: Option<Vec<ModelCount>>,
}

#[derive(Deserialize, Clone)]
struct ModelCount {
    name: String,
    request_count: u64,
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
    let dst = target.join("ollama-usage-tray.png");
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
        if u.ok {
            format!("${:.0}/${:.0}", u.used_amount(), u.plan_total)
        } else {
            "…".into()
        }
    }

    fn icon_name(&self) -> String {
        let u = self.usage.lock().unwrap();
        if !u.ok {
            return "network-offline".into();
        }
        "ollama-usage-tray".into()
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
        if !u.ok {
            tt.description = format!(
                "Sem dados{}\nAguardando o próximo ciclo de atualização.",
                u.err
                    .as_ref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
            return tt;
        }
        tt.description = format_tooltip(&u);
        tt
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let u = self.usage.lock().unwrap();
        let reqs: String = if u.requests.is_empty() {
            "-".into()
        } else {
            u.requests
                .iter()
                .map(|(n, c)| format!("{n}: {c}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        drop(u);
        vec![
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
            StandardItem {
                label: {
                    let u = self.usage.lock().unwrap();
                    format!("Plano: {}", u.plan)
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: {
                    let u = self.usage.lock().unwrap();
                    format!("Créditos do plano: ${:.0}/mês", u.plan_total)
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: {
                    let u = self.usage.lock().unwrap();
                    if !u.ok {
                        "Usado: …".into()
                    } else if u.legacy_limits() {
                        format!(
                            "Usado (7 dias): ${:.2} de ${:.0} ({:.0}%)",
                            u.weekly_used(),
                            u.plan_total,
                            u.weekly_frac * 100.0
                        )
                    } else {
                        format!(
                            "Usado (mês): ${:.2} de ${:.0} ({:.0}%)",
                            u.used_amount(),
                            u.plan_total,
                            u.plan_frac() * 100.0
                        )
                    }
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: {
                    let u = self.usage.lock().unwrap();
                    if !u.ok {
                        "Sessão atual: …".into()
                    } else if u.legacy_limits() {
                        format!("Sessão atual: {:.1}%", u.session_frac * 100.0)
                    } else {
                        let period = u
                            .monthly_period
                            .as_deref()
                            .map(|p| {
                                // "last_4_weeks" -> "últimas 4 semanas"
                                p.replace("last_4_weeks", "últimas 4 semanas")
                                    .replace("current_month", "mês atual")
                            });
                        format!(
                            "Sessão atual: —  (período: {})",
                            period.unwrap_or("mensal".into())
                        )
                    }
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("Requests (semana): {reqs}"),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("Renova em: {}", reset_label()),
                enabled: false,
                ..Default::default()
            }
            .into(),
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
        ]
    }
}

fn format_tooltip(u: &Usage) -> String {
    let mut s = String::new();
    if u.legacy_limits() {
        // modelo antigo: limites de sessão/semana
        s.push_str(&format!("Plano: {}\n", u.plan));
        s.push_str(&format!("Créditos: ${:.0}/mês\n", u.plan_total));
        s.push_str(&format!(
            "Usado (7 dias): ${:.2} ({:.1}% dos créditos)\n",
            u.weekly_used(),
            u.weekly_frac * 100.0
        ));
        s.push_str(&format!("Sessão atual: {:.1}%\n", u.session_frac * 100.0));
        s.push_str(&format!("Disponível: ${:.2}\n", u.weekly_left()));
    } else {
        // modelo novo: créditos mensais em $
        s.push_str(&format!("Plano: {}\n", u.plan));
        s.push_str(&format!("Créditos: ${:.0}/mês\n", u.plan_total));
        s.push_str(&format!(
            "Usado (mês): ${:.2} ({:.1}%)\n",
            u.used_amount(),
            u.plan_frac() * 100.0
        ));
        s.push_str(&format!("Disponível: ${:.2}\n", u.remaining()));
    }
    s.push_str(&format!("Renova: {}", reset_label()));
    s.push_str(&format!("\nAPI key: {}", u.masked_key));
    if !u.requests.is_empty() {
        let reqs: Vec<String> = u
            .requests
            .iter()
            .map(|(n, c)| format!("{n}: {c}"))
            .collect();
        s.push_str(&format!("\nRequests: {}", reqs.join(", ")));
    }
    s
}

// ---------------------------------------------------------------- reset date

fn reset_label() -> String {
    let days = days_until_reset();
    let date = next_reset_date_str();
    match days {
        0 => "hoje".into(),
        1 => format!("amanhã ({date})"),
        n => format!("{n} dias ({date})"),
    }
}

fn days_until_reset() -> u64 {
    let now = unix_now();
    let (_, _, d) = epoch_to_ymd(now);
    if (d as i64) < RESET_DAY as i64 {
        (RESET_DAY as i64 - d as i64) as u64
    } else if d == RESET_DAY {
        0
    } else {
        days_in_current_month_left(now)
    }
}

fn days_in_current_month_left(now: i64) -> u64 {
    let (y, m, d) = epoch_to_ymd(now);
    let dim = days_in_month(y, m);
    (dim as i64 - d as i64 + RESET_DAY as i64) as u64
}

fn next_reset_date_str() -> String {
    let now = unix_now();
    let (y, m, d) = epoch_to_ymd(now);
    let nm = if d >= RESET_DAY {
        if m == 12 { 1 } else { m + 1 }
    } else {
        m
    };
    let _ = y;
    format!("{RESET_DAY:02}/{nm:02}")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// days in month (m: 1-12)
fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
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

// ---------------------------------------------------------------- plan totals

fn plan_total_for(plan: &str) -> f64 {
    match plan.to_ascii_lowercase().as_str() {
        "max" => 300.0,
        "pro" => 60.0,
        "team" => 1000.0,
        _ => 300.0,
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

fn fetch_usage(key: &str, _key_source: &str) -> Result<Usage, String> {
    // native-tls: usa o keystore do sistema (obrigatório em rede com CA corporativa, ex.: Netskope)
    let tls = native_tls::TlsConnector::new()
        .map_err(|e| format!("tls: {e}"))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(Duration::from_secs(15))
        .build();

    // plan name (POST /api/me)
    let me: MeResp = agent
        .post("https://ollama.com/api/me")
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string("{}")
        .map_err(|e| format!("api/me: {e}"))?
        .into_json()
        .map_err(|e| format!("parse me: {e}"))?;
    let plan = me.plan.unwrap_or_else(|| "max".into());
    let plan_total = plan_total_for(&plan);

    // usage fractions (GET /api/usage)
    let resp = agent
        .get("https://ollama.com/api/usage")
        .set("Authorization", &format!("Bearer {key}"))
        .call()
        .map_err(|e| format!("api/usage: {e}"))?;
    let body: UsageResp = resp
        .into_json()
        .map_err(|e| format!("parse usage: {e}"))?;
    let limits = body
        .limits
        .ok_or("resposta sem limits")?;
    let weekly_frac = limits.weekly.as_ref().and_then(|w| w.usage).unwrap_or(0.0);
    let session_frac = limits.session.as_ref().and_then(|s| s.usage).unwrap_or(0.0);
    let has_session_weekly = limits.session.is_some() || limits.weekly.is_some();
    // requests: novo modelo pode usar limits.session; antigo usa weekly
    let requests_models = if let Some(w) = &limits.weekly {
        w.models.clone()
    } else {
        limits.session.as_ref().and_then(|s| s.models.clone())
    };
    let requests: Vec<(String, u64)> = requests_models
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.name, m.request_count))
        .collect();

    // ---- pricing model detection ----
    // antigo: limits.session/weekly presentes (frações do plano) + activity.cost == 0
    // novo: créditos mensais em $ -> session/weekly ausentes, activity.cost = $ usados no mês
    let monthly_cost: Option<f64> = if has_session_weekly {
        None
    } else {
        body.activity
            .as_ref()
            .and_then(|a| a.cost.as_ref())
            .and_then(|c| c.parse::<f64>().ok())
    };
    let monthly_period: Option<String> = body
        .activity
        .as_ref()
        .and_then(|a| a.period.as_ref())
        .and_then(|p| p.typ.clone());

    Ok(Usage {
        ok: true,
        err: None,
        plan,
        plan_total,
        weekly_frac,
        session_frac,
        requests,
        masked_key: mask_key(key),
        monthly_cost,
        has_session_weekly,
        monthly_period,
    })
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
                        println!("plano: {} (${:.0}/mês)", u.plan, u.plan_total);
                        println!("usado (7 dias): ${:.2} ({:.1}%)", u.weekly_used(), u.weekly_frac * 100.0);
                        println!("sessão: {:.1}%", u.session_frac * 100.0);
                        println!("renova: {}", reset_label());
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
    loop {
        if quit.load(Ordering::SeqCst) {
            return;
        }
        if Instant::now() >= next_fetch {
            let fresh = fetch_usage(&key, &key_source);
            let mut u = usage.lock().unwrap();
            match fresh {
                Ok(nu) => *u = nu,
                Err(e) => {
                    u.ok = false;
                    u.err = Some(e);
                }
            }
            next_fetch = Instant::now() + interval;
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(_) => next_fetch = Instant::now(), // manual refresh
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}