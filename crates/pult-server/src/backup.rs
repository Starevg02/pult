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

/// Сколько копий хранить.
pub const KEEP: usize = 14;

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
        ("PGHOST", u.host_str().unwrap_or("localhost").to_string()),
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

/// Делает копию; незавершённая копия не остаётся среди готовых.
pub async fn run(c: &BackupConfig) -> Result<(PathBuf, u64), String> {
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
    let result = make(c, &tmp).await;
    let size = match result {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    let done = c.dir.join(&name);
    std::fs::rename(&tmp, &done).map_err(|e| e.to_string())?;
    prune(&c.dir, KEEP).map_err(|e| e.to_string())?;
    Ok((done, size))
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

/// Делает копию и записывает результат. Сбой — уведомление суперадминистраторам.
pub async fn run_and_record(c: &BackupConfig, db: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    let now = OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).ok();
    let status = match run(c).await {
        Ok((path, size_bytes)) => {
            tracing::info!(path = %path.display(), size_bytes, "резервная копия создана");
            BackupStatus { last_at: now, ok: true, path: Some(path.display().to_string()), size_bytes, error: None }
        }
        Err(e) => {
            tracing::error!(error = %e, "резервная копия не создана");
            pult_core::notify::notify_superadmins(db, "system.backup_failed", serde_json::json!({"error": e})).await?;
            BackupStatus { last_at: now, ok: false, error: Some(e), ..Default::default() }
        }
    };
    pult_core::settings::save(db, &status, None).await
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
