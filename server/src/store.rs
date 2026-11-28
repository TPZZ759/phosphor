// Config + viewer state, both JSON files shared with the original app (same formats).
use chrono::{Datelike, Days, Local, NaiveDate};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);
const BACKUP_KEEP_DAYS: u64 = 14;
const HEALTH_PRUNE_MS: i64 = 14 * 86_400_000;

/// Companion `.bak` path for a file (e.g. `state.json` -> `state.json.bak`).
pub fn bak_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".bak");
    PathBuf::from(os)
}

fn is_critical_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("state.json" | "config.json" | "meta.json")
    )
}

fn looks_like_valid_backup_source(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    let len = meta.len();
    if len < 2 {
        return false;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        // state.json and config.json are small: verify JSON object validity before overwriting .bak.
        Some("state.json" | "config.json") => std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map_or(false, |v| v.is_object()),
        // Larger JSON files (e.g. meta.json): quick non-empty JSON brace check.
        _ => std::fs::read(path).ok().map_or(false, |b| {
            let trimmed = b.iter().copied().skip_while(|c| c.is_ascii_whitespace()).next();
            let tail = b.iter().rev().copied().skip_while(|c| c.is_ascii_whitespace()).next();
            matches!((trimmed, tail), (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']')))
        }),
    }
}

fn rename_with_retry(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..5 {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Windows sharing / lock / transient access-denied errors (5, 32, 33).
                let retryable = matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33))
                    || e.kind() == std::io::ErrorKind::PermissionDenied;
                last_err = Some(e);
                if !retryable || attempt == 4 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "rename failed")))
}

fn unique_tmp_path(path: &Path) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp_name = format!(".{name}.{pid}.{seq}.tmp");
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

/// Crash-proof atomic file write:
/// 1. Writes payload to a unique `.tmp` sibling file and flushes/syncs to disk.
/// 2. For critical files (`state.json`, `config.json`, `meta.json`), atomically snapshots
///    a valid existing target into `<file>.bak` before replacing it.
/// 3. Replaces the target with up to 5 Windows sharing-violation retries (25ms backoff).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let tmp = unique_tmp_path(path);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        let _ = f.sync_all();
    }

    if is_critical_file(path) && path.exists() && looks_like_valid_backup_source(path) {
        let bak = bak_path(path);
        let bak_tmp = unique_tmp_path(&bak);
        if std::fs::copy(path, &bak_tmp).is_ok() {
            if rename_with_retry(&bak_tmp, &bak).is_err() {
                let _ = std::fs::remove_file(&bak_tmp);
            }
        } else {
            let _ = std::fs::remove_file(&bak_tmp);
        }
    }

    match rename_with_retry(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn read_json_object(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| format!("JSON parse error: {e}"))?;
    if !v.is_object() {
        return Err("expected top-level JSON object".into());
    }
    Ok(v)
}

fn backup_dir_for(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.join("backups"))
        .unwrap_or_else(|| PathBuf::from("backups"))
}

fn parse_backup_date(file_name: &str) -> Option<NaiveDate> {
    let rest = file_name.strip_prefix("state-")?.strip_suffix(".json")?;
    NaiveDate::parse_from_str(rest, "%Y-%m-%d").ok()
}

fn newest_daily_backups(path: &Path) -> Vec<PathBuf> {
    let dir = backup_dir_for(path);
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut dated: Vec<(NaiveDate, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?;
            let d = parse_backup_date(name)?;
            Some((d, p))
        })
        .collect();
    dated.sort_by(|a, b| b.0.cmp(&a.0));
    dated.into_iter().map(|(_, p)| p).collect()
}

fn load_json_with_fallback(path: &Path, label: &str, check_daily_backups: bool) -> Value {
    let primary_exists = path.exists();
    if primary_exists {
        match read_json_object(path) {
            Ok(v) => return v,
            Err(err) => {
                eprintln!(
                    "[store] ERROR: failed to load {label} ({}): {err} — attempting automatic .bak recovery!",
                    path.display()
                );
            }
        }
    }

    let bak = bak_path(path);
    if bak.exists() {
        match read_json_object(&bak) {
            Ok(v) => {
                eprintln!(
                    "[store] RECOVERED {label} from backup {} (primary was missing or corrupt)",
                    bak.display()
                );
                return v;
            }
            Err(err) => {
                eprintln!(
                    "[store] ERROR: backup {} also failed to parse: {err}",
                    bak.display()
                );
            }
        }
    }

    if check_daily_backups {
        for bp in newest_daily_backups(path) {
            if let Ok(v) = read_json_object(&bp) {
                eprintln!(
                    "[store] RECOVERED {label} from rolling daily backup {}",
                    bp.display()
                );
                return v;
            }
        }
    }

    if primary_exists {
        eprintln!(
            "[store] CRITICAL: {label} ({}) and all backups could not be parsed; starting with empty state.",
            path.display()
        );
    }
    json!({})
}

fn prune_old_backups(dir: &Path, today: NaiveDate, keep_days: u64) {
    let cutoff = today.checked_sub_days(Days::new(keep_days)).unwrap_or(today);
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if let Some(d) = parse_backup_date(name) {
                if d < cutoff {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
}

fn prune_old_health(v: &mut Value, now: i64) {
    let Some(health) = v.get_mut("health").and_then(|h| h.as_object_mut()) else {
        return;
    };
    health.retain(|_, entry| {
        let ts = entry["t"].as_i64().unwrap_or(0);
        ts <= 0 || now.saturating_sub(ts) <= HEALTH_PRUNE_MS
    });
}

pub struct Config {
    pub path: PathBuf,
    pub v: Mutex<Value>,
}
impl Config {
    pub fn load(path: PathBuf) -> Self {
        let v = load_json_with_fallback(&path, "config.json", false);
        Config { path, v: Mutex::new(v) }
    }
    pub fn get(&self, k: &str) -> Value { self.v.lock().unwrap().get(k).cloned().unwrap_or(Value::Null) }
    pub fn str(&self, k: &str) -> String { crate::util::s(&self.get(k)) }
    pub fn set(&self, k: &str, val: Value) {
        let mut g = self.v.lock().unwrap();
        g[k] = val;
        if let Ok(body) = serde_json::to_string_pretty(&*g) {
            if let Err(e) = atomic_write(&self.path, body.as_bytes()) {
                eprintln!("[store] ERROR: failed to save {}: {e}", self.path.display());
            }
        }
    }
    pub fn servers(&self) -> Vec<String> {
        self.get("servers").as_array().map(|a| a.iter().map(crate::util::s).collect()).unwrap_or_default()
    }
}

/// Viewer state (progress, lists, ratings...). Saved with a short debounce, crash-proof atomic
/// writes, `.bak` fallback recovery, and automated rolling 14-day daily backups.
pub struct State {
    pub path: PathBuf,
    pub v: Mutex<Value>,
    dirty: AtomicBool,
    last_backup_day: Mutex<String>,
}
impl State {
    pub fn load(path: PathBuf) -> Self {
        let mut v = load_json_with_fallback(&path, "state.json", true);
        for (k, d) in [
            ("progress", json!({})), ("list", json!({})), ("history", json!([])), ("favs", json!({})),
            ("ratings", json!({})), ("follows", json!({})), ("newEps", json!({})), ("teams", json!([])),
            ("notified", json!({})), ("prefs", json!({"hide": []})), ("watched", json!({})),
            ("wanted", json!({})), ("arrived", json!({})), ("savedRows", json!([])),
            ("recentLive", json!([])), ("reminders", json!([])), ("cwDismissed", json!({})),
            ("cwPinned", json!({})), ("dubWatch", json!({})),
        ] {
            if v.get(k).is_none() { v[k] = d; }
        }
        if v["prefs"].get("hide").is_none() { v["prefs"]["hide"] = json!([]); }
        State {
            path,
            v: Mutex::new(v),
            dirty: Default::default(),
            last_backup_day: Mutex::new(String::new()),
        }
    }
    pub fn with<R>(&self, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut g = self.v.lock().unwrap();
        f(&mut g)
    }
    pub fn read<R>(&self, f: impl FnOnce(&Value) -> R) -> R {
        let g = self.v.lock().unwrap();
        f(&g)
    }
    pub fn save(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    fn write_daily_backup_for(&self, bytes: &[u8], today: NaiveDate) -> Option<PathBuf> {
        let day_str = format!("{:04}-{:02}-{:02}", today.year(), today.month(), today.day());
        let dir = backup_dir_for(&self.path);
        let dest = dir.join(format!("state-{day_str}.json"));
        if let Err(e) = atomic_write(&dest, bytes) {
            eprintln!("[store] ERROR: failed to write daily backup {}: {e}", dest.display());
            return None;
        }
        prune_old_backups(&dir, today, BACKUP_KEEP_DAYS);
        *self.last_backup_day.lock().unwrap() = day_str;
        Some(dest)
    }

    fn maybe_daily_backup(&self, bytes: &[u8]) {
        let today = Local::now().date_naive();
        let day_str = format!("{:04}-{:02}-{:02}", today.year(), today.month(), today.day());
        {
            let g = self.last_backup_day.lock().unwrap();
            if *g == day_str {
                return;
            }
        }
        let dir = backup_dir_for(&self.path);
        let dest = dir.join(format!("state-{day_str}.json"));
        if dest.exists() {
            prune_old_backups(&dir, today, BACKUP_KEEP_DAYS);
            *self.last_backup_day.lock().unwrap() = day_str;
            return;
        }
        let _ = self.write_daily_backup_for(bytes, today);
    }

    /// Force an immediate backup snapshot into `data/backups/state-YYYY-MM-DD.json` and prune > 14d.
    pub fn backup_now(&self) -> Option<PathBuf> {
        let body = {
            let mut g = self.v.lock().unwrap();
            prune_old_health(&mut g, crate::util::now_ms());
            serde_json::to_vec(&*g).ok()?
        };
        let _ = atomic_write(&self.path, &body);
        self.dirty.store(false, Ordering::SeqCst);
        self.write_daily_backup_for(&body, Local::now().date_naive())
    }

    pub fn flush(&self) {
        if self.dirty.swap(false, Ordering::SeqCst) {
            let body = {
                let mut g = self.v.lock().unwrap();
                prune_old_health(&mut g, crate::util::now_ms());
                match serde_json::to_vec(&*g) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("[store] ERROR: failed to serialize state.json: {e}");
                        return;
                    }
                }
            };
            match atomic_write(&self.path, &body) {
                Ok(()) => self.maybe_daily_backup(&body),
                Err(e) => {
                    eprintln!("[store] ERROR: failed to save {}: {e}", self.path.display());
                    self.dirty.store(true, Ordering::SeqCst);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tv_store_test_{tag}_{}_{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn atomic_write_creates_bak_and_config_state_recover_from_corrupt_primary() {
        let dir = temp_dir("recover_bak");

        // Config: write v1, then write v2 -> .bak holds v1; corrupt primary -> loads v1!
        let cfg_path = dir.join("config.json");
        let cfg = Config::load(cfg_path.clone());
        cfg.set("username", json!("alice"));
        cfg.set("port", json!(7879));
        assert!(bak_path(&cfg_path).exists(), "config.json.bak should be created on overwrite");

        std::fs::write(&cfg_path, "{corrupt json").unwrap();
        let recovered_cfg = Config::load(cfg_path.clone());
        assert_eq!(recovered_cfg.str("username"), "alice");

        // State: write v1, then write v2 -> .bak holds v1; corrupt primary -> recovers watch history!
        let st_path = dir.join("state.json");
        let st = State::load(st_path.clone());
        st.with(|s| {
            s["watched"]["movie:603"] = json!({ "t": 1234, "title": "The Matrix" });
        });
        st.save();
        st.flush();
        st.with(|s| {
            s["watched"]["movie:155"] = json!({ "t": 5678, "title": "The Dark Knight" });
        });
        st.save();
        st.flush();
        assert!(bak_path(&st_path).exists(), "state.json.bak should be created");

        std::fs::write(&st_path, "truncated{").unwrap();
        let recovered_st = State::load(st_path.clone());
        assert!(recovered_st.read(|s| s["watched"].get("movie:603").is_some()));

        // If BOTH primary and .bak are corrupt, State::load falls back to daily backup!
        std::fs::write(bak_path(&st_path), "also broken").unwrap();
        let recovered_daily = State::load(st_path.clone());
        assert!(recovered_daily.read(|s| s["watched"].get("movie:603").is_some()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daily_backups_are_created_and_old_backups_pruned() {
        let dir = temp_dir("daily_prune");
        let st_path = dir.join("state.json");
        let bdir = dir.join("backups");
        std::fs::create_dir_all(&bdir).unwrap();

        let today = Local::now().date_naive();
        let old_date = today.checked_sub_days(Days::new(20)).unwrap();
        let recent_date = today.checked_sub_days(Days::new(5)).unwrap();
        let old_file = bdir.join(format!("state-{}.json", old_date.format("%Y-%m-%d")));
        let recent_file = bdir.join(format!("state-{}.json", recent_date.format("%Y-%m-%d")));
        let unrelated_file = bdir.join("notes.txt");
        std::fs::write(&old_file, "{}").unwrap();
        std::fs::write(&recent_file, "{}").unwrap();
        std::fs::write(&unrelated_file, "keep").unwrap();

        let st = State::load(st_path.clone());
        st.with(|s| s["prefs"]["city"] = json!("Sydney"));
        let created = st.backup_now().expect("backup_now should return path");
        assert!(created.exists());
        assert!(!old_file.exists(), "backups older than 14 days must be pruned");
        assert!(recent_file.exists(), "backups within 14 days must be kept");
        assert!(unrelated_file.exists(), "non-backup files must never be deleted");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_prunes_decayed_health_entries_older_than_14_days() {
        let dir = temp_dir("health_prune");
        let st_path = dir.join("state.json");
        let now = crate::util::now_ms();
        let st = State::load(st_path.clone());
        st.with(|s| {
            s["health"] = json!({
                "100": { "f": 3, "t": now - 20 * 86_400_000 },
                "200": { "f": 1, "t": now - 2 * 86_400_000 }
            });
        });
        st.save();
        st.flush();

        let reloaded = State::load(st_path);
        assert!(reloaded.read(|s| s["health"].get("100").is_none()), "20d-old health must be pruned");
        assert!(reloaded.read(|s| s["health"].get("200").is_some()), "2d-old health must remain");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
