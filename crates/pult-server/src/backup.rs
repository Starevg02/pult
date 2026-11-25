//! Резервная копия: дамп базы, вложения и сертификат — в отдельный каталог.
//! `pult backup` и ежедневная задача. Ключ шифрования (`secret.key`) хранится отдельно от копий: без него
//! пароли служебных учёток в дампе не расшифровать — и тому, кто получил копию, тоже.

use pult_core::{Secret, settings::SettingsGroup};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use time::OffsetDateTime;

/// Сколько копий хранить, если политика не задана.
pub const KEEP: u32 = 14;
/// Предел хранения: больше копий — повод для внешнего хранилища, а не для каталога на сервере портала.
pub const MAX_KEEP: u32 = 365;

/// Политика копий: сколько хранить и проверять ли каждую.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct BackupPolicy {
    /// Сколько последних копий хранить (1–365).
    pub keep: u32,
    /// Проверять каждую копию: `pg_restore --list` читает дамп целиком.
    pub verify: bool,
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self { keep: KEEP, verify: true }
    }
}

impl SettingsGroup for BackupPolicy {
    const KEY: &'static str = "backup_policy";
}

impl BackupPolicy {
    pub fn valid(&self) -> bool {
        (1..=MAX_KEEP).contains(&self.keep)
    }
}

#[derive(Clone)]
pub struct BackupConfig {
    pub dir: PathBuf,
    pub data_dir: Option<PathBuf>,
    pub files_dir: PathBuf,
    pub database_url: Secret,
}

/// Последняя копия — для админки.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct BackupStatus {
    /// RFC 3339.
    pub last_at: Option<String>,
    pub ok: bool,
    pub path: Option<String>,
    pub size_bytes: u64,
    pub error: Option<String>,
    /// Дамп прочитан `pg_restore --list` без ошибок.
    pub verified: bool,
    /// Сколько объектов базы в дампе (по оглавлению `pg_restore --list`).
    pub objects: Option<u64>,
    /// Сколько копий хранится по политике.
    pub keep: Option<u32>,
}

/// Готовая копия.
#[derive(Debug)]
pub struct Made {
    pub path: PathBuf,
    pub size: u64,
    /// Объектов в оглавлении дампа, если копия проверялась.
    pub objects: Option<u64>,
}

impl SettingsGroup for BackupStatus {
    const KEY: &'static str = "backup_status";
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    let mut total = 0;
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            if e.file_name() != "backups" {
                total += copy_tree(&e.path(), &target)?;
            }
        } else if e.path().extension().is_none_or(|x| x != "tmp") {
            total += std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(total)
}

/// Параметры подключения — через окружение: в аргументах пароль виден в списке процессов.
fn pg_env(url: &str) -> Result<Vec<(&'static str, String)>, String> {
    let u = url::Url::parse(url).map_err(|e| format!("адрес базы: {e}"))?;
    let decode = |s: &str| url_decode(s);
    let mut env = vec![
        // Каталог сокета (`%2Fvar%2Frun%2Fpostgresql`) приходит закодированным.
        ("PGHOST", decode(u.host_str().unwrap_or("localhost"))),
        ("PGPORT", u.port().unwrap_or(5432).to_string()),
        ("PGUSER", decode(u.username())),
        ("PGDATABASE", u.path().trim_start_matches('/').to_string()),
    ];
    if let Some(p) = u.password() {
        env.push(("PGPASSWORD", decode(p)));
    }
    if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "sslmode") {
        env.push(("PGSSLMODE", v.to_string()));
    }
    Ok(env)
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn prune(root: &Path, keep: usize) -> std::io::Result<()> {
    let mut dirs: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("pult-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dirs.sort();
    for (_, d) in dirs.iter().rev().skip(keep) {
        std::fs::remove_dir_all(d)?;
    }
    Ok(())
}

fn private_dir(p: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Делает копию; незавершённая или не прошедшая проверку копия не остаётся среди готовых.
pub async fn run(c: &BackupConfig, policy: &BackupPolicy) -> Result<Made, String> {
    let now = OffsetDateTime::now_utc();
    let name = format!(
        "pult-{}{:02}{:02}-{:02}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    private_dir(&c.dir).map_err(|e| format!("{}: {e}", c.dir.display()))?;
    let tmp = c.dir.join(format!(".{name}.partial"));
    let _ = std::fs::remove_dir_all(&tmp);
    private_dir(&tmp).map_err(|e| e.to_string())?;
    let result = match make(c, &tmp).await {
        Ok(size) if policy.verify => verify(&tmp.join("db.dump")).await.map(|n| (size, Some(n))),
        Ok(size) => Ok((size, None)),
        Err(e) => Err(e),
    };
    let (size, objects) = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    let done = c.dir.join(&name);
    std::fs::rename(&tmp, &done).map_err(|e| e.to_string())?;
    let keep = usize::try_from(policy.keep.clamp(1, MAX_KEEP)).unwrap_or(KEEP as usize);
    prune(&c.dir, keep).map_err(|e| e.to_string())?;
    Ok(Made { path: done, size, objects })
}

/// Проверка копии: `pg_restore --list` читает весь дамп и строит оглавление. Обрезанный или испорченный
/// файл даёт ошибку. Возвращает число объектов в оглавлении.
async fn verify(dump: &Path) -> Result<u64, String> {
    let mut cmd = tokio::process::Command::new("pg_restore");
    // Подключение к базе не нужно: оглавление строится из самого файла.
    cmd.arg("--list").arg(dump).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(1800), cmd.output())
        .await
        .map_err(|_| "проверка копии не уложилась в 30 минут".to_string())?
        .map_err(|e| format!("pg_restore не запустился ({e}): нужен клиент PostgreSQL"))?;
    if !out.status.success() {
        return Err(format!(
            "копия не прошла проверку: {}",
            String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()
        ));
    }
    let objects = toc_entries(&String::from_utf8_lossy(&out.stdout));
    if objects == 0 {
        return Err("копия не прошла проверку: в дампе нет объектов".into());
    }
    Ok(objects)
}

/// Строки оглавления `pg_restore --list`: комментарии начинаются с `;`.
fn toc_entries(list: &str) -> u64 {
    list.lines().filter(|l| !l.trim().is_empty() && !l.starts_with(';')).count() as u64
}

async fn make(c: &BackupConfig, tmp: &Path) -> Result<u64, String> {
    let dump = tmp.join("db.dump");
    let mut cmd = tokio::process::Command::new("pg_dump");
    cmd.arg("--format=custom").arg("--file").arg(&dump).kill_on_drop(true);
    for (k, v) in pg_env(c.database_url.expose())? {
        cmd.env(k, v);
    }
    let out = tokio::time::timeout(Duration::from_secs(3600), cmd.output())
        .await
        .map_err(|_| "pg_dump не уложился в час".to_string())?
        .map_err(|e| format!("pg_dump не запустился ({e}): нужен клиент PostgreSQL"))?;
    if !out.status.success() {
        return Err(format!("pg_dump: {}", String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()));
    }
    let mut size = std::fs::metadata(&dump).map_err(|e| e.to_string())?.len();
    let (files_dir, data_dir, to) = (c.files_dir.clone(), c.data_dir.clone(), tmp.to_path_buf());
    size += tokio::task::spawn_blocking(move || copy_files(&files_dir, data_dir.as_deref(), &to))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("копирование файлов: {e}"))?;
    Ok(size)
}

/// Вложения и сертификат портала. Ключ шифрования (`secret.key`) в копию не кладётся: его хранят отдельно,
/// иначе укравший копию расшифрует пароли служебных учёток (AD, почта, АТС).
fn copy_files(files_dir: &Path, data_dir: Option<&Path>, to: &Path) -> std::io::Result<u64> {
    let mut n = 0;
    if files_dir.exists() {
        n += copy_tree(files_dir, &to.join("files"))?;
    }
    if let Some(d) = data_dir
        && d.join("tls").exists()
    {
        n += copy_tree(&d.join("tls"), &to.join("tls"))?;
    }
    Ok(n)
}

/// Делает копию по политике из настроек и записывает результат. Сбой — уведомление суперадминистраторам.
pub async fn run_and_record(c: &BackupConfig, db: &sqlx::PgPool) -> Result<BackupStatus, sqlx::Error> {
    let policy: BackupPolicy = pult_core::settings::load(db).await?;
    let now = OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).ok();
    let status = match run(c, &policy).await {
        Ok(m) => {
            {
                tracing::info!(path = %m.path.display(), size_bytes = m.size, objects = ?m.objects, "резервная копия создана");
            }
            BackupStatus {
                last_at: now,
                ok: true,
                path: Some(m.path.display().to_string()),
                size_bytes: m.size,
                error: None,
                verified: m.objects.is_some(),
                objects: m.objects,
                keep: Some(policy.keep),
            }
        }
        Err(e) => {
            {
                tracing::error!(error = %e, "резервная копия не создана");
            }
            pult_core::notify::notify_superadmins(db, "system.backup_failed", serde_json::json!({"error": e})).await?;
            BackupStatus { last_at: now, ok: false, error: Some(e), keep: Some(policy.keep), ..Default::default() }
        }
    };
    pult_core::settings::save(db, &status, None).await?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_comes_from_environment() {
        let env = pg_env("postgres://pult:p%40ss%2Fx@db.example.test:6543/pult?sslmode=require").unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("PGHOST"), Some("db.example.test"));
        assert_eq!(get("PGPORT"), Some("6543"));
        assert_eq!(get("PGUSER"), Some("pult"));
        assert_eq!(get("PGPASSWORD"), Some("p@ss/x"), "пароль раскодирован");
        assert_eq!(get("PGDATABASE"), Some("pult"));
        assert_eq!(get("PGSSLMODE"), Some("require"));
        let env = pg_env("postgres://pult@%2Fvar%2Frun%2Fpostgresql/pult").unwrap();
        assert_eq!(env.iter().find(|(n, _)| *n == "PGHOST").map(|(_, v)| v.as_str()), Some("/var/run/postgresql"));
    }

    #[test]
    fn toc_counts_entries_not_comments() {
        let list = ";\n; Archive created at 2026-11-20\n;     dbname: pult\n;\n215; 1259 16390 TABLE public users pult\n3401; 0 16390 TABLE DATA public users pult\n\n";
        assert_eq!(toc_entries(list), 2);
        assert_eq!(toc_entries(";\n; empty\n"), 0);
    }

    #[test]
    fn policy_bounds() {
        assert!(BackupPolicy::default().valid() && BackupPolicy::default().verify);
        assert!(!BackupPolicy { keep: 0, verify: true }.valid());
        assert!(!BackupPolicy { keep: MAX_KEEP + 1, verify: true }.valid());
        let p: BackupPolicy = serde_json::from_value(serde_json::json!({"keep": 30})).unwrap();
        assert_eq!((p.keep, p.verify), (30, true), "недостающие поля — по умолчанию");
    }

    #[test]
    fn old_copies_are_removed_and_others_kept() {
        let d = std::env::temp_dir().join(format!("pult-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for i in 0..4 {
            std::fs::create_dir_all(d.join(format!("pult-{i}"))).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::create_dir_all(d.join("other")).unwrap();
        prune(&d, 2).unwrap();
        let mut left: Vec<String> =
            std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["other", "pult-2", "pult-3"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn encryption_key_stays_out_of_backup() {
        // Копию уносят на другой сервер: с ключом внутри она раскрыла бы пароли служебных учёток.
        let d = std::env::temp_dir().join(format!("pult-nokey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("data/tls")).unwrap();
        std::fs::create_dir_all(d.join("files/ab")).unwrap();
        std::fs::create_dir_all(d.join("out")).unwrap();
        std::fs::write(d.join("data/secret.key"), "ключ").unwrap();
        std::fs::write(d.join("data/tls/pult.crt"), "cert").unwrap();
        std::fs::write(d.join("files/ab/f"), "file").unwrap();
        copy_files(&d.join("files"), Some(&d.join("data")), &d.join("out")).unwrap();
        assert!(!d.join("out/secret.key").exists(), "ключа в копии нет");
        assert!(d.join("out/tls/pult.crt").exists() && d.join("out/files/ab/f").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tree_copy_skips_backups_and_temp_files() {
        let d = std::env::temp_dir().join(format!("pult-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/ab")).unwrap();
        std::fs::create_dir_all(d.join("src/backups")).unwrap();
        std::fs::write(d.join("src/ab/f"), "12345").unwrap();
        std::fs::write(d.join("src/ab/f.tmp"), "x").unwrap();
        std::fs::write(d.join("src/backups/old"), "x").unwrap();
        assert_eq!(copy_tree(&d.join("src"), &d.join("dst")).unwrap(), 5);
        assert!(d.join("dst/ab/f").exists() && !d.join("dst/ab/f.tmp").exists() && !d.join("dst/backups").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
