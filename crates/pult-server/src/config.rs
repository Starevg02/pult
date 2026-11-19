//! Конфигурация `pult.toml` + переопределение переменными окружения (ADR-0003).
//!
//! Инфраструктура — в файле и переменных окружения, прикладные настройки — в БД.

use pult_ad::AdConfig;
use pult_core::Secret;
use serde::Deserialize;
use sqlx::types::ipnetwork::IpNetwork;
use std::path::{Path, PathBuf};

/// Минимальная длина ключа шифрования секретов в БД.
const MIN_SECRET_KEY_LEN: usize = 32;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Адрес и порт HTTP, например `0.0.0.0:8080`.
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub log_format: LogFormat,
    /// Ключ шифрования секретов в БД (`PULT_SECRET_KEY`). Без него сервер не стартует.
    pub secret_key: Option<Secret>,
    pub database: Option<DatabaseConfig>,
    pub ad: Option<AdConfig>,
    /// Токен для `/metrics` с других адресов (`PULT_METRICS_TOKEN`, заголовок `Authorization: Bearer …`).
    /// Без него метрики отдаются только с самого сервера (127.0.0.1, ::1).
    pub metrics_token: Option<Secret>,
    #[serde(default)]
    pub jobs: JobsConfig,
    /// Встроенный HTTPS. Без секции — HTTP (например, за обратным прокси).
    pub tls: Option<TlsConfig>,
    /// Первый суперадминистратор (`PULT_INITIAL_SUPERADMIN=login` или `DOMAIN\login`): получает роль
    /// при первом входе, только если суперадминистраторов ещё нет.
    pub initial_superadmin: Option<String>,
    /// Cookie сеанса без `Secure` и `__Host-` — только для разработки по HTTP (`PULT_INSECURE_COOKIES=1`).
    /// За обратным прокси с HTTPS не нужно: браузер видит HTTPS.
    #[serde(default)]
    pub insecure_cookies: bool,
    /// Куда складывать резервные копии (`PULT_BACKUP_DIR`); по умолчанию — `backups` в каталоге данных.
    pub backup_dir: Option<PathBuf>,
    /// Почта без TLS — только для разработки и CI (`PULT_INSECURE_MAIL=1`).
    #[serde(default)]
    pub insecure_mail: bool,
    /// Каталог данных (`PULT_DATA_DIR`): ключ шифрования (`secret.key`) и сертификат портала.
    /// Если задан, а `PULT_SECRET_KEY` нет — ключ создаётся здесь сам при первом запуске.
    pub data_dir: Option<PathBuf>,
    /// HTTPS без настройки (`PULT_AUTO_TLS=1`): при первом запуске — временный сертификат в каталоге
    /// данных, свой загружается в админке. Нужен `data_dir`; не действует, если задан `[tls]`.
    #[serde(default)]
    pub auto_tls: bool,
    /// Адреса обратных прокси (`PULT_TRUSTED_PROXIES=10.0.0.5,10.1.0.0/16`): только им верим в
    /// `X-Forwarded-For`. Без этого за прокси все пользователи выглядят одним адресом.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

/// Пути к сертификату (цепочка PEM) и ключу (PEM). Переменные: `PULT_TLS_CERT`, `PULT_TLS_KEY`.
/// Вторая пара (`extra_cert`, `extra_key`; `PULT_TLS_EXTRA_CERT`, `PULT_TLS_EXTRA_KEY`) — для другого
/// имени портала: её получает клиент, запросивший имя из этого сертификата, остальные — основную.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub cert: std::path::PathBuf,
    pub key: std::path::PathBuf,
    #[serde(default)]
    pub extra_cert: Option<std::path::PathBuf>,
    #[serde(default)]
    pub extra_key: Option<std::path::PathBuf>,
}

impl TlsConfig {
    /// Вторая пара, если задана целиком.
    pub fn extra(&self) -> Option<(&Path, &Path)> {
        Some((self.extra_cert.as_deref()?, self.extra_key.as_deref()?))
    }
}

/// Фоновые задачи: исполнитель очереди и расписания.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JobsConfig {
    /// `false` — эта копия сервера задачи не выполняет (например, отдельная копия только для веба).
    pub enabled: bool,
    /// Сколько задач выполняется одновременно.
    pub concurrency: usize,
    /// Смещение, в котором читаются расписания cron, например `+03:00`.
    pub utc_offset: String,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self { enabled: true, concurrency: 4, utc_offset: "+03:00".into() }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Читаемый текст — для разработки.
    #[default]
    Text,
    /// JSON — для сервера и сбора журналов.
    Json,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Строка подключения содержит пароль, поэтому — секрет.
    pub url: Secret,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_listen() -> String {
    "127.0.0.1:8080".into()
}

fn default_max_connections() -> u32 {
    10
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать {path}: {source}")]
    Read { path: String, source: std::io::Error },
    #[error("ошибка в {path}: {message}")]
    Parse { path: String, message: String },
    #[error(
        "не задан ключ шифрования: укажите PULT_SECRET_KEY (не короче {MIN_SECRET_KEY_LEN} символов, например `openssl rand -base64 48`)"
    )]
    MissingSecretKey,
    #[error("ключ шифрования короче {MIN_SECRET_KEY_LEN} символов или оставлен шаблонный CHANGEME")]
    WeakSecretKey,
    #[error("не задано подключение к PostgreSQL: секция [database] или PULT_DATABASE_URL")]
    MissingDatabase,
    #[error("не задано подключение к AD: секция [ad]")]
    MissingAd,
    #[error("секция [jobs]: {0}")]
    Jobs(String),
    #[error("не удалось подставить пароль из файла в адрес PostgreSQL: {0}")]
    DatabaseUrl(String),
    #[error("адрес доверенного прокси «{0}» — нужен IP или сеть в формате CIDR")]
    TrustedProxy(String),
    #[error("PULT_AUTO_TLS требует каталог данных: задайте PULT_DATA_DIR")]
    AutoTlsWithoutDataDir,
    #[error(
        "вторая пара TLS: задайте и сертификат, и ключ (PULT_TLS_EXTRA_CERT, PULT_TLS_EXTRA_KEY) вместе с основной"
    )]
    TlsExtra,
}

/// Файл только для владельца; `create_new` — двое не создадут его одновременно.
pub fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

fn read_or_create_key(path: &Path) -> std::io::Result<String> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match create_private(path) {
        Ok(mut f) => {
            let key = pult_core::crypto::random_token(48).map_err(std::io::Error::other)?;
            f.write_all(key.as_bytes())?;
            f.sync_all()?;
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(std::fs::read_to_string(path)?.trim().to_string())
        }
        Err(e) => Err(e),
    }
}

/// Источник переменных окружения — подменяется в тестах.
pub trait Env {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
}

impl Config {
    /// Читает файл (если есть) и применяет переменные `PULT_*`.
    /// Явно указанный файл обязан существовать; `pult.toml` по умолчанию — нет.
    pub fn load(path: Option<&Path>, env: &dyn Env) -> Result<Self, ConfigError> {
        let (path, required) = match path {
            Some(p) => (p.to_path_buf(), true),
            None => (Path::new("pult.toml").to_path_buf(), false),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(ConfigError::Read { path: path.display().to_string(), source }),
        };
        let mut cfg: Config = toml::from_str(&text)
            .map_err(|e| ConfigError::Parse { path: path.display().to_string(), message: e.message().to_string() })?;
        cfg.apply_env(env)?;
        Ok(cfg)
    }

    fn apply_env(&mut self, env: &dyn Env) -> Result<(), ConfigError> {
        if let Some(v) = env.get("PULT_LISTEN") {
            self.listen = v;
        }
        if let Some(v) = env.get("PULT_LOG_FORMAT") {
            self.log_format = if v.eq_ignore_ascii_case("json") { LogFormat::Json } else { LogFormat::Text };
        }
        if let Some(v) = env.get("PULT_SECRET_KEY") {
            self.secret_key = Some(Secret::new(v));
        }
        if let Some(v) = env.get("PULT_DATABASE_URL") {
            match &mut self.database {
                Some(db) => db.url = Secret::new(v),
                None => {
                    self.database =
                        Some(DatabaseConfig { url: Secret::new(v), max_connections: default_max_connections() })
                }
            }
        }
        // Пароль отдельным файлом — так его передают Docker и системы секретов.
        if let Some(path) = env.get("PULT_DATABASE_PASSWORD_FILE") {
            let password = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read { path, source })?;
            let db = self.database.as_mut().ok_or(ConfigError::MissingDatabase)?;
            let mut url = url::Url::parse(db.url.expose()).map_err(|e| ConfigError::DatabaseUrl(e.to_string()))?;
            url.set_password(Some(password.trim()))
                .map_err(|()| ConfigError::DatabaseUrl("в адресе нельзя задать пароль".into()))?;
            db.url = Secret::new(url.to_string());
        }
        if let Some(v) = env.get("PULT_BACKUP_DIR") {
            self.backup_dir = Some(v.into());
        }
        if let Some(v) = env.get("PULT_INSECURE_MAIL") {
            self.insecure_mail = matches!(v.as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env.get("PULT_DATA_DIR") {
            self.data_dir = Some(v.into());
        }
        if let Some(v) = env.get("PULT_AUTO_TLS") {
            self.auto_tls = matches!(v.as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env.get("PULT_TRUSTED_PROXIES") {
            self.trusted_proxies = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        if env.get("PULT_AGENT_TOKEN").is_some() {
            // Агенты регистрируются сами: общий токен больше не нужен.
            tracing::warn!("PULT_AGENT_TOKEN больше не используется: агенты регистрируются токенами из админки");
        }
        if let Some(v) = env.get("PULT_METRICS_TOKEN").filter(|v| !v.is_empty()) {
            self.metrics_token = Some(Secret::new(v));
        }
        match (env.get("PULT_TLS_CERT"), env.get("PULT_TLS_KEY")) {
            (Some(cert), Some(key)) => {
                let (extra_cert, extra_key) = self.tls.take().map(|t| (t.extra_cert, t.extra_key)).unwrap_or_default();
                self.tls = Some(TlsConfig { cert: cert.into(), key: key.into(), extra_cert, extra_key });
            }
            (Some(cert), None) => {
                if let Some(t) = self.tls.as_mut() {
                    t.cert = cert.into();
                }
            }
            (None, Some(key)) => {
                if let Some(t) = self.tls.as_mut() {
                    t.key = key.into();
                }
            }
            (None, None) => {}
        }
        for (var, is_cert) in [("PULT_TLS_EXTRA_CERT", true), ("PULT_TLS_EXTRA_KEY", false)] {
            if let Some(v) = env.get(var).filter(|v| !v.is_empty()) {
                let t = self.tls.as_mut().ok_or(ConfigError::TlsExtra)?;
                *(if is_cert { &mut t.extra_cert } else { &mut t.extra_key }) = Some(v.into());
            }
        }
        if let (Some(v), Some(ad)) = (env.get("PULT_AD_BIND_PASSWORD"), self.ad.as_mut()) {
            ad.bind_password = Secret::new(v);
        }
        if let Some(v) = env.get("PULT_INITIAL_SUPERADMIN") {
            self.initial_superadmin = Some(v);
        }
        if let Some(v) = env.get("PULT_INSECURE_COOKIES") {
            self.insecure_cookies = matches!(v.as_str(), "1" | "true" | "yes");
        }
        Ok(())
    }

    /// Ключа в настройках нет, а каталог данных есть — читает `secret.key` или создаёт его.
    pub fn ensure_secret_key(&mut self) -> Result<(), ConfigError> {
        if self.secret_key.is_some() {
            return Ok(());
        }
        let Some(dir) = self.data_dir.clone() else { return Ok(()) };
        let key = read_or_create_key(&dir.join("secret.key"))
            .map_err(|source| ConfigError::Read { path: dir.join("secret.key").display().to_string(), source })?;
        self.secret_key = Some(Secret::new(key));
        Ok(())
    }

    /// Разобранный список доверенных прокси.
    pub fn trusted_proxies(&self) -> Result<Vec<IpNetwork>, ConfigError> {
        self.trusted_proxies.iter().map(|s| s.parse().map_err(|_| ConfigError::TrustedProxy(s.clone()))).collect()
    }

    /// Требования для `pult serve`.
    pub fn validate_for_serve(&self) -> Result<(), ConfigError> {
        let key = self.secret_key.as_ref().ok_or(ConfigError::MissingSecretKey)?;
        if key.expose().len() < MIN_SECRET_KEY_LEN || key.expose().contains("CHANGEME") {
            return Err(ConfigError::WeakSecretKey);
        }
        self.trusted_proxies()?;
        if self.tls.as_ref().is_some_and(|t| t.extra_cert.is_some() != t.extra_key.is_some()) {
            return Err(ConfigError::TlsExtra);
        }
        if self.auto_tls && self.tls.is_none() && self.data_dir.is_none() {
            return Err(ConfigError::AutoTlsWithoutDataDir);
        }
        pult_jobs::cron::parse_offset(&self.jobs.utc_offset).map_err(|e| ConfigError::Jobs(e.to_string()))?;
        if self.jobs.concurrency == 0 {
            return Err(ConfigError::Jobs("concurrency должен быть больше 0".into()));
        }
        Ok(())
    }

    pub fn database(&self) -> Result<&DatabaseConfig, ConfigError> {
        self.database.as_ref().ok_or(ConfigError::MissingDatabase)
    }

    pub fn ad(&self) -> Result<&AdConfig, ConfigError> {
        self.ad.as_ref().ok_or(ConfigError::MissingAd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MapEnv(HashMap<&'static str, &'static str>);
    impl Env for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|v| v.to_string())
        }
    }

    fn write(dir: &Path, text: &str) -> std::path::PathBuf {
        let p = dir.join("pult.toml");
        std::fs::write(&p, text).unwrap();
        p
    }

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pult-cfg-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn secret_key_created_once_in_data_dir() {
        let d = tmp();
        let mut c = Config::load(None, &MapEnv(HashMap::new())).unwrap();
        c.ensure_secret_key().unwrap();
        assert!(c.secret_key.is_none(), "без каталога данных ключ сам не появляется");
        c.data_dir = Some(d.join("data"));
        c.ensure_secret_key().unwrap();
        let first = c.secret_key.clone().unwrap().expose().to_string();
        assert!(first.len() >= MIN_SECRET_KEY_LEN);
        c.secret_key = None;
        c.ensure_secret_key().unwrap();
        assert_eq!(c.secret_key.unwrap().expose(), first, "при следующем запуске — тот же ключ");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join("data/secret.key")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn database_password_from_file_is_encoded() {
        let d = tmp();
        let f = d.join("pw");
        std::fs::write(&f, "p@ss/word\n").unwrap();
        let path: &'static str = Box::leak(f.display().to_string().into_boxed_str());
        let env = MapEnv(HashMap::from([
            ("PULT_DATABASE_URL", "postgres://pult@db:5432/pult"),
            ("PULT_DATABASE_PASSWORD_FILE", path),
        ]));
        let c = Config::load(None, &env).unwrap();
        assert_eq!(c.database.unwrap().url.expose(), "postgres://pult:p%40ss%2Fword@db:5432/pult");
    }

    #[test]
    fn trusted_proxies_are_validated() {
        let env = MapEnv(HashMap::from([("PULT_TRUSTED_PROXIES", "10.0.0.5, 192.0.2.0/24")]));
        assert_eq!(Config::load(None, &env).unwrap().trusted_proxies().unwrap().len(), 2);
        let env = MapEnv(HashMap::from([("PULT_TRUSTED_PROXIES", "proxy.local")]));
        assert!(Config::load(None, &env).unwrap().trusted_proxies().is_err());
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn env_overrides_file() {
        let d = tmp();
        let p = write(&d, "listen = \"0.0.0.0:9000\"\n[database]\nurl = \"postgres://file\"\n");
        let env = MapEnv(HashMap::from([("PULT_DATABASE_URL", "postgres://env"), ("PULT_LOG_FORMAT", "json")]));
        let c = Config::load(Some(&p), &env).unwrap();
        assert_eq!(c.listen, "0.0.0.0:9000");
        assert_eq!(c.log_format, LogFormat::Json);
        assert_eq!(c.database().unwrap().url.expose(), "postgres://env");
    }

    #[test]
    fn missing_default_file_is_fine_but_explicit_is_not() {
        let env = MapEnv(HashMap::new());
        assert!(Config::load(Some(Path::new("definitely-missing.toml")), &env).is_err());
    }

    #[test]
    fn serve_requires_strong_key() {
        let env = MapEnv(HashMap::new());
        let d = tmp();
        let p = write(&d, "");
        let mut c = Config::load(Some(&p), &env).unwrap();
        assert!(matches!(c.validate_for_serve(), Err(ConfigError::MissingSecretKey)));
        c.secret_key = Some(Secret::new("CHANGEME-CHANGEME-CHANGEME-CHANGEME"));
        assert!(matches!(c.validate_for_serve(), Err(ConfigError::WeakSecretKey)));
        c.secret_key = Some(Secret::new("short"));
        assert!(matches!(c.validate_for_serve(), Err(ConfigError::WeakSecretKey)));
        c.secret_key = Some(Secret::new("x".repeat(48)));
        assert!(c.validate_for_serve().is_ok());
    }

    #[test]
    fn tls_from_file_and_env() {
        let d = tmp();
        let none = Config::load(Some(&write(&d, "")), &MapEnv(HashMap::new())).unwrap();
        assert!(none.tls.is_none(), "без секции — HTTP");
        let env =
            MapEnv(HashMap::from([("PULT_TLS_CERT", "/etc/pult/tls/c.crt"), ("PULT_TLS_KEY", "/etc/pult/tls/k.key")]));
        let c = Config::load(Some(&write(&d, "")), &env).unwrap();
        let t = c.tls.unwrap();
        assert_eq!((t.cert.to_str(), t.key.to_str()), (Some("/etc/pult/tls/c.crt"), Some("/etc/pult/tls/k.key")));
        let file = "[tls]\ncert = \"a.crt\"\nkey = \"a.key\"\n";
        let env = MapEnv(HashMap::from([("PULT_TLS_KEY", "b.key")]));
        let t = Config::load(Some(&write(&d, file)), &env).unwrap().tls.unwrap();
        assert_eq!((t.cert.to_str(), t.key.to_str()), (Some("a.crt"), Some("b.key")), "переменная меняет одно поле");
        assert!(Config::load(Some(&write(&d, "[tls]\ncert = \"a\"\n")), &MapEnv(HashMap::new())).is_err(), "без key");
    }

    #[test]
    fn second_tls_pair_from_file_and_env() {
        let d = tmp();
        let key = format!("secret_key = \"{}\"\n", "x".repeat(48));
        let file =
            format!("{key}[tls]\ncert = \"a.crt\"\nkey = \"a.key\"\nextra_cert = \"x.crt\"\nextra_key = \"x.key\"\n");
        let c = Config::load(Some(&write(&d, &file)), &MapEnv(HashMap::new())).unwrap();
        let t = c.tls.as_ref().unwrap();
        assert_eq!(t.extra(), Some((Path::new("x.crt"), Path::new("x.key"))));
        assert!(c.validate_for_serve().is_ok());

        // Основная пара из переменных не теряет вторую из файла; переменные второй пары её меняют.
        let env = MapEnv(HashMap::from([
            ("PULT_TLS_CERT", "/etc/pult/tls/c.crt"),
            ("PULT_TLS_KEY", "/etc/pult/tls/k.key"),
            ("PULT_TLS_EXTRA_KEY", "/etc/pult/tls/w.key"),
        ]));
        let t = Config::load(Some(&write(&d, &file)), &env).unwrap().tls.unwrap();
        assert_eq!(t.extra(), Some((Path::new("x.crt"), Path::new("/etc/pult/tls/w.key"))));

        let env = MapEnv(HashMap::from([
            ("PULT_TLS_CERT", "c.crt"),
            ("PULT_TLS_KEY", "k.key"),
            ("PULT_TLS_EXTRA_CERT", "w.crt"),
        ]));
        let half = Config::load(Some(&write(&d, &key)), &env).unwrap();
        assert!(half.tls.as_ref().unwrap().extra().is_none());
        assert!(matches!(half.validate_for_serve(), Err(ConfigError::TlsExtra)), "только сертификат");

        let env = MapEnv(HashMap::from([("PULT_TLS_EXTRA_CERT", "w.crt"), ("PULT_TLS_EXTRA_KEY", "w.key")]));
        assert!(matches!(Config::load(Some(&write(&d, &key)), &env), Err(ConfigError::TlsExtra)), "без основной");
    }

    #[test]
    fn jobs_section_defaults_and_validation() {
        let env = MapEnv(HashMap::new());
        let d = tmp();
        let key = format!("secret_key = \"{}\"\n", "x".repeat(48));
        let c = Config::load(Some(&write(&d, &key)), &env).unwrap();
        assert!(c.jobs.enabled);
        assert_eq!((c.jobs.concurrency, c.jobs.utc_offset.as_str()), (4, "+03:00"));
        let c = Config::load(Some(&write(&d, &format!("{key}[jobs]\nutc_offset = \"MSK\"\n"))), &env).unwrap();
        assert!(matches!(c.validate_for_serve(), Err(ConfigError::Jobs(_))));
        let c = Config::load(Some(&write(&d, &format!("{key}[jobs]\nconcurrency = 0\n"))), &env).unwrap();
        assert!(matches!(c.validate_for_serve(), Err(ConfigError::Jobs(_))));
        assert!(Config::load(Some(&write(&d, &format!("{key}[jobs]\nworkers = 2\n"))), &env).is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let d = tmp();
        let p = write(&d, "listn = \"typo\"\n");
        assert!(matches!(Config::load(Some(&p), &MapEnv(HashMap::new())), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn debug_hides_secrets() {
        let d = tmp();
        let p = write(
            &d,
            "secret_key = \"key-very-secret-0123456789-abcdefgh\"\n[database]\nurl = \"postgres://u:dbpass@h/db\"\n",
        );
        let c = Config::load(Some(&p), &MapEnv(HashMap::new())).unwrap();
        let s = format!("{c:?}");
        assert!(!s.contains("dbpass") && !s.contains("key-very-secret"), "{s}");
    }
}
