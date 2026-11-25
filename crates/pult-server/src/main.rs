//! `pult` — один бинарник платформы.

mod api;
mod backup;
mod config;
#[cfg(feature = "demo")]
mod demo;
mod edge;
mod http;
mod jobs;
mod migrate;
mod negotiate;
mod openapi;
mod setup;
mod static_files;
mod telemetry;
mod tls;

use anyhow::Context;
use clap::{Parser, Subcommand};
use config::{Config, ProcessEnv};
use pult_ad::{AdError, DcPool};
use pult_core::Secret;
use sqlx::postgres::PgPoolOptions;
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(name = "pult", version, about = "Pult — пульт IT-службы")]
struct Cli {
    /// Файл конфигурации (по умолчанию ./pult.toml, если есть).
    #[arg(long, global = true, env = "PULT_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Запустить веб-сервер.
    Serve,
    /// Применить миграции БД одной транзакцией (под advisory lock): сбой откатывает всё обновление.
    Migrate,
    /// Проверить подключения.
    Check {
        #[command(subcommand)]
        what: CheckWhat,
    },
    /// Резервная копия: база, вложения, сертификат (ключ шифрования хранится отдельно).
    Backup,
    /// Перенос данных из старых систем.
    Import {
        #[command(subcommand)]
        what: ImportWhat,
    },
    /// Первичная настройка: подключение к AD (пароль служебной учётки спросит без отображения).
    Setup {
        #[command(subcommand)]
        what: SetupWhat,
    },
    /// Демо: поддельный домен с вымышленными сотрудниками и сервер на нём (сборка с `--features demo`).
    #[cfg(feature = "demo")]
    Demo,
    /// Вывести схему OpenAPI (JSON) — для генерации TS-клиента.
    Openapi,
    /// Показать версию.
    Version,
    /// Конфигурация пограничного прокси для публикации портала в Интернет.
    Edge {
        #[command(subcommand)]
        what: EdgeWhat,
    },
}

#[derive(Subcommand)]
enum EdgeWhat {
    /// Вывести конфигурацию nginx для узла в DMZ (сохраните в /etc/nginx/conf.d/pult-edge.conf).
    Nginx {
        /// Внешнее имя портала, например pult.example.com.
        #[arg(long)]
        host: String,
        /// Внутренний адрес портала, например https://10.0.0.10.
        #[arg(long)]
        upstream: String,
        /// Сертификат внешнего имени (с цепочкой) на узле прокси.
        #[arg(long)]
        cert: String,
        /// Ключ сертификата на узле прокси.
        #[arg(long)]
        key: String,
        /// CA внутреннего сертификата портала: прокси проверит, что говорит именно с порталом.
        #[arg(long)]
        upstream_ca: Option<String>,
        /// Наибольший размер запроса, МБ (больше лимита вложений заявок).
        #[arg(long, default_value_t = 26)]
        body_mb: u32,
        /// Попыток входа в минуту с одного адреса.
        #[arg(long, default_value_t = 10)]
        login_per_minute: u32,
    },
}

#[derive(Subcommand)]
enum CheckWhat {
    /// PostgreSQL: подключение и миграции.
    Db,
    /// Active Directory: служебная учётка, вход пользователя, синхронизация.
    Ad {
        /// Проверить вход этим логином; пароль спросит без отображения.
        #[arg(long)]
        login: Option<String>,
        /// Инкрементальная синхронизация от этого USN вместо полной.
        #[arg(long)]
        since_usn: Option<u64>,
        /// Размер страницы при синхронизации.
        #[arg(long, default_value_t = 500)]
        page_size: i32,
    },
}

#[derive(Subcommand)]
enum SetupWhat {
    /// Домен AD: проверить каждый контроллер и сохранить подключение в БД (пароль — зашифрованным).
    /// Дальше домен меняется в админке; повторный запуск заменяет настройки.
    Ad {
        /// Базовый DN: `DC=corp,DC=example,DC=local`.
        #[arg(long)]
        base_dn: String,
        /// Контроллеры через запятую в порядке приоритета: `dc1.corp.example.local,dc2.corp.example.local`.
        #[arg(long, value_delimiter = ',')]
        controllers: Vec<String>,
        /// Служебная учётка только на чтение (UPN или DN).
        #[arg(long)]
        bind_dn: String,
        /// NetBIOS-имя домена (`CORP`).
        #[arg(long)]
        netbios: Option<String>,
        /// `ldaps` (по умолчанию) или `starttls`.
        #[arg(long, default_value = "ldaps")]
        tls: String,
        /// PEM CA домена, если его нет в системном хранилище.
        #[arg(long)]
        ca_cert: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ImportWhat {
    /// PhonebookDB (SQL Server): прочитать и показать отчёт переноса.
    Phonebook {
        /// Строка ADO.NET без пароля: `Server=tcp:host,1433;Database=PhonebookDB;Encrypt=true`.
        #[arg(long, env = "PULT_PHONEBOOK_MSSQL", required_unless_present = "from_snapshot")]
        mssql: Option<String>,
        /// SQL-логин только на чтение.
        #[arg(long, env = "PULT_PHONEBOOK_USER", conflicts_with = "windows_auth")]
        user: Option<String>,
        /// Только прочитать PhonebookDB и сохранить снимок в JSON (без БД Pult) — когда SQL Server
        /// доступен с одной машины, а БД Pult — с другой. В файле персональные данные: удалить после переноса.
        #[arg(long, conflicts_with = "from_snapshot")]
        save_snapshot: Option<PathBuf>,
        /// Перенести из ранее сохранённого снимка вместо чтения SQL Server.
        #[arg(long)]
        from_snapshot: Option<PathBuf>,
        /// Windows-вход без пароля: на Windows — текущая учётка (SSPI), на Linux — Kerberos
        /// (билет из `kinit` или keytab в `KRB5_CLIENT_KTNAME`).
        #[arg(long)]
        windows_auth: bool,
        /// PEM-сертификат CA SQL Server.
        #[arg(long)]
        ca_cert: Option<PathBuf>,
        /// Не проверять сертификат SQL Server (самоподписанный, без CA). Только с --dry-run;
        /// для настоящего переноса серверу нужен сертификат от доверенного CA.
        #[arg(long, conflicts_with = "ca_cert")]
        insecure_skip_tls_verify: bool,
        /// Выполнить перенос и откатить его: отчёт и сверка без записи в БД Pult.
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Command::Openapi = cli.command {
        say(&serde_json::to_string_pretty(&openapi::spec())?);
        return Ok(());
    }
    if let Command::Version = cli.command {
        say(&format!("pult {}", env!("CARGO_PKG_VERSION")));
        return Ok(());
    }
    if let Command::Edge {
        what: EdgeWhat::Nginx { host, upstream, cert, key, upstream_ca, body_mb, login_per_minute },
    } = cli.command
    {
        let o = edge::EdgeOptions { host, upstream, cert, key, upstream_ca, body_mb, login_per_minute };
        say(&edge::nginx(&o).map_err(anyhow::Error::msg)?);
        return Ok(());
    }
    let mut cfg = Config::load(cli.config.as_deref(), &ProcessEnv)?;
    telemetry::init_tracing(cfg.log_format);
    cfg.ensure_secret_key()?;
    match cli.command {
        Command::Serve => serve(cfg).await,
        #[cfg(feature = "demo")]
        Command::Demo => demo::run(cfg).await,
        Command::Migrate => migrate(&cfg).await,
        Command::Backup => {
            // Политика (сколько хранить, проверять ли) и итог — в базе, как у ежедневной задачи.
            let pool = db_pool(&cfg, false).await?;
            let s = backup::run_and_record(&backup_config(&cfg)?, &pool).await?;
            if !s.ok {
                anyhow::bail!(s.error.unwrap_or_default());
            }
            say(&format!("Резервная копия: {} ({} байт)", s.path.unwrap_or_default(), s.size_bytes));
            if let Some(n) = s.objects {
                say(&format!("Проверена: в дампе {n} объектов"));
            }
            if let Some(d) = &cfg.data_dir {
                say(&format!(
                    "Ключ шифрования в копию не входит — храните его отдельно: {}",
                    d.join("secret.key").display()
                ));
            }
            Ok(())
        }
        Command::Check { what: CheckWhat::Db } => check_db(&cfg).await,
        Command::Check { what: CheckWhat::Ad { login, since_usn, page_size } } => {
            check_ad(&cfg, login, since_usn, page_size).await
        }
        Command::Setup { what: SetupWhat::Ad { base_dn, controllers, bind_dn, netbios, tls, ca_cert } } => {
            setup_ad(&cfg, base_dn, controllers, bind_dn, netbios, tls, ca_cert).await
        }
        Command::Import {
            what:
                ImportWhat::Phonebook {
                    mssql,
                    user,
                    save_snapshot,
                    from_snapshot,
                    windows_auth,
                    ca_cert,
                    insecure_skip_tls_verify,
                    dry_run,
                },
        } => {
            let args = PhonebookArgs { mssql, user, windows_auth, ca_cert, skip_tls_verify: insecure_skip_tls_verify };
            import_phonebook(&cfg, args, save_snapshot, from_snapshot, dry_run).await
        }
        Command::Version | Command::Openapi | Command::Edge { .. } => unreachable!(),
    }
}

/// Вывод для человека в терминале (команды `check`, `version`).
#[allow(clippy::print_stdout)]
fn say(line: &str) {
    println!("{line}");
}

async fn db_pool(cfg: &Config, lazy: bool) -> anyhow::Result<sqlx::PgPool> {
    let db = cfg.database()?;
    let opts = PgPoolOptions::new().max_connections(db.max_connections).acquire_timeout(Duration::from_secs(5));
    let pool = if lazy {
        opts.connect_lazy(db.url.expose())?
    } else {
        opts.connect(db.url.expose()).await.context("подключение к PostgreSQL")?
    };
    Ok(pool)
}

/// Ядро для команд и сервера: БД, ключ шифрования секретов, политика cookie, разрешения модулей.
fn core_state(cfg: &Config, db: sqlx::PgPool) -> anyhow::Result<pult_core::Core> {
    let key = cfg.secret_key.as_ref().ok_or(config::ConfigError::MissingSecretKey)?;
    if cfg.insecure_cookies {
        tracing::warn!("cookie сеанса без Secure (PULT_INSECURE_COOKIES) — только для разработки по HTTP");
    }
    if cfg.insecure_mail {
        tracing::warn!("разрешена почта без TLS (PULT_INSECURE_MAIL) — только для разработки");
    }
    Ok(pult_core::Core {
        db,
        secrets: Arc::new(pult_core::crypto::SecretBox::new(key)),
        cookies: pult_core::session::CookiePolicy { secure: !cfg.insecure_cookies },
        permissions: Arc::new(api::permissions()),
        events: pult_core::events::Hub::default(),
        insecure_mail: cfg.insecure_mail,
        org: Default::default(),
    })
}

fn backup_config(cfg: &Config) -> anyhow::Result<backup::BackupConfig> {
    let base = cfg.data_dir.clone().unwrap_or_else(|| PathBuf::from("."));
    Ok(backup::BackupConfig {
        dir: cfg.backup_dir.clone().unwrap_or_else(|| base.join("backups")),
        data_dir: cfg.data_dir.clone(),
        files_dir: base.join("files"),
        database_url: cfg.database()?.url.clone(),
    })
}

/// Начальные данные, которым нужна БД: права системных ролей, домен из `[ad]`.
/// БД может быть ещё недоступна — тогда повторяем, не мешая серверу стартовать.
fn spawn_bootstrap(st: api::ApiState, ad: Option<pult_ad::AdConfig>) {
    tokio::spawn(async move {
        loop {
            let r: anyhow::Result<()> = async {
                let n = pult_core::rbac::seed_defaults(&st.core.db, &st.core.permissions).await?;
                if n > 0 {
                    tracing::info!(added = n, "системным ролям выданы разрешения по умолчанию");
                }
                if let Some(ad) = &ad {
                    pult_people::domain::seed_from_config(&st.core, ad, None).await?;
                }
                if setup::needed(&st).await?.0 {
                    // Кодом, а не полем журнала: так его легко найти в `docker compose logs`.
                    tracing::warn!(
                        "SETUP CODE: {} — первый запуск: откройте портал и введите этот код",
                        st.setup.code()
                    );
                }
                Ok(())
            }
            .await;
            match r {
                Ok(()) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "начальная настройка не удалась — повтор через 10 с");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    });
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    cfg.validate_for_serve()?;
    // Ленивое подключение: сервер стартует и при недоступной БД, а /health это покажет.
    let db = db_pool(&cfg, true).await?;
    // Сертификат проверяется до открытия порта: с битым сертификатом сервер не стартует.
    let managed = match (&cfg.tls, cfg.auto_tls, &cfg.data_dir) {
        (None, true, Some(dir)) => {
            Some(tls::ManagedTls::open(&dir.join("tls"), vec![jobs::host(), "localhost".into()])?)
        }
        _ => None,
    };
    let api_state = api::ApiState {
        core: core_state(&cfg, db.clone())?,
        people: pult_people::PeopleState {
            ad: pult_people::domain::AdHandle::default(),
            initial_superadmin: cfg.initial_superadmin.clone(),
        },
        helpdesk: pult_helpdesk::HelpdeskState {
            files_dir: cfg.data_dir.clone().unwrap_or_else(|| PathBuf::from(".")).join("files"),
        },
        backup: backup_config(&cfg)?,
        setup: setup::SetupState::new()?,
        tls: managed.clone(),
    };
    spawn_bootstrap(api_state.clone(), cfg.ad.clone());
    api_state.core.events.spawn_listener(db.clone());
    spawn_push(api_state.core.clone());
    let events = api_state.core.events.clone();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let runner = if cfg.jobs.enabled {
        let registry = jobs::registry(&api_state)?;
        Some(tokio::spawn(pult_jobs::runner::run(
            db.clone(),
            registry,
            jobs::runner_config(&cfg.jobs)?,
            stopped.clone(),
        )))
    } else {
        tracing::info!("фоновые задачи на этой копии выключены ([jobs] enabled = false)");
        None
    };
    let state = http::AppState {
        db: Some(db),
        metrics: telemetry::metrics_handle(),
        metrics_token: cfg.metrics_token.clone(),
        trusted_proxies: Arc::new(cfg.trusted_proxies()?),
    };
    let tls =
        match (&cfg.tls, managed) {
            (Some(t), _) => Some(axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(
                tls::server_config_with(&t.cert, &t.key, t.extra())?,
            ))),
            (None, Some(m)) => Some(m.rustls),
            (None, None) => None,
        };
    let listener = tokio::net::TcpListener::bind(&cfg.listen).await.with_context(|| format!("порт {}", cfg.listen))?;
    tracing::info!(listen = %cfg.listen, https = tls.is_some(), version = env!("CARGO_PKG_VERSION"), "pult запущен");
    tokio::spawn(async move {
        shutdown_signal().await;
        events.stop();
        let _ = stop.send(true);
    });
    let mut http_stopped = stopped.clone();
    // Один сервер для HTTP и HTTPS: axum::serve, как и axum-server, не ставит hyper таймер, поэтому
    // ограничение на чтение заголовков задаём сами (`limit_slow_clients`).
    let handle = axum_server::Handle::new();
    let on_stop = handle.clone();
    tokio::spawn(async move {
        let _ = http_stopped.wait_for(|s| *s).await;
        on_stop.graceful_shutdown(Some(Duration::from_secs(20)));
    });
    let std_listener = listener.into_std()?;
    match tls {
        None => {
            let mut server = axum_server::from_tcp(std_listener)?.handle(handle);
            http::limit_slow_clients(server.http_builder(), http::HEADER_READ_TIMEOUT);
            server
                .serve(
                    http::router(state, Some(api_state)).into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await?
        }
        Some(tls) => {
            // HSTS: браузер, однажды открывший портал по HTTPS, больше не пойдёт на него по HTTP.
            let app =
                http::router(state, Some(api_state)).layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
                    axum::http::header::STRICT_TRANSPORT_SECURITY,
                    axum::http::HeaderValue::from_static("max-age=31536000"),
                ));
            let mut server = axum_server::from_tcp_rustls(std_listener, tls)?.handle(handle);
            http::limit_slow_clients(server.http_builder(), http::HEADER_READ_TIMEOUT);
            server.serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?
        }
    }
    if let Some(runner) = runner {
        let _ = runner.await;
    }
    tracing::info!("pult остановлен");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

async fn migrate(cfg: &Config) -> anyhow::Result<()> {
    let pool = db_pool(cfg, false).await?;
    // Все недостающие миграции — одной транзакцией: сломанная откатывает всё обновление.
    let n = migrate::run(&pool, &sqlx::migrate!("../../migrations")).await?;
    tracing::info!(applied = n, "миграции применены");
    Ok(())
}

async fn check_db(cfg: &Config) -> anyhow::Result<()> {
    let pool = db_pool(cfg, false).await?;
    let version: String = sqlx::query_scalar("SHOW server_version").fetch_one(&pool).await?;
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations").fetch_one(&pool).await.unwrap_or(0);
    let known = sqlx::migrate!("../../migrations").iter().count();
    say(&format!("PostgreSQL {version}: подключение есть, миграций применено {applied} из {known}"));
    Ok(())
}

/// Как читать PhonebookDB.
struct PhonebookArgs {
    mssql: Option<String>,
    user: Option<String>,
    windows_auth: bool,
    ca_cert: Option<PathBuf>,
    skip_tls_verify: bool,
}

/// Источник для журнала — без логина и пароля: только сервер и база.
fn source_name(ado: &str) -> String {
    ado.split(';')
        .filter(|p| {
            let k = p.split('=').next().unwrap_or("").trim().to_lowercase();
            matches!(k.as_str(), "server" | "data source" | "database" | "initial catalog")
        })
        .collect::<Vec<_>>()
        .join(";")
}

async fn read_phonebook(a: PhonebookArgs) -> anyhow::Result<(pult_phonebook::Snapshot, String)> {
    use pult_phonebook::reader::Login;
    let mssql = a.mssql.ok_or_else(|| anyhow::anyhow!("укажите --mssql или --from-snapshot"))?;
    let (windows_auth, user, ca_cert, skip_tls_verify) = (a.windows_auth, a.user, a.ca_cert, a.skip_tls_verify);
    if skip_tls_verify {
        tracing::warn!(
            "сертификат SQL Server НЕ проверяется (--insecure-skip-tls-verify): подмену сервера в сети не обнаружить; \
             для настоящего переноса нужен сертификат от доверенного CA"
        );
    }
    let login = match (windows_auth, user) {
        (true, _) => Login::Windows,
        (false, Some(user)) => {
            let password = match std::env::var("PULT_PHONEBOOK_PASSWORD") {
                Ok(p) if !p.is_empty() => Secret::new(p),
                _ => Secret::new(rpassword::prompt_password(format!("Пароль SQL для {user}: "))?),
            };
            Login::Sql { user, password }
        }
        (false, None) => anyhow::bail!("укажите --user или --windows-auth"),
    };
    let src = pult_phonebook::reader::Source { ado: mssql.clone(), login, ca_cert, skip_tls_verify };
    let snapshot = pult_phonebook::reader::read(&src).await?;
    say(&format!("tls_verified: {}", !skip_tls_verify));
    Ok((snapshot, source_name(&mssql)))
}

/// Снимок в файле: исходные строки PhonebookDB и откуда они.
#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotFile {
    source: String,
    snapshot: pult_phonebook::Snapshot,
}

async fn import_phonebook(
    cfg: &Config,
    args: PhonebookArgs,
    save_snapshot: Option<PathBuf>,
    from_snapshot: Option<PathBuf>,
    dry_run: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        dry_run || !args.skip_tls_verify,
        "--insecure-skip-tls-verify допустим только с --dry-run: для переноса серверу нужен сертификат от доверенного CA"
    );
    let insecure = args.skip_tls_verify;
    if insecure {
        tracing::warn!("сертификат SQL Server НЕ проверяется (--insecure-skip-tls-verify)");
    }
    let (snapshot, source) = match &from_snapshot {
        Some(p) => {
            let f: SnapshotFile = serde_json::from_slice(&std::fs::read(p).with_context(|| p.display().to_string())?)
                .with_context(|| format!("{}: не снимок PhonebookDB", p.display()))?;
            (f.snapshot, format!("{} (снимок)", f.source))
        }
        None => read_phonebook(args).await?,
    };
    if let Some(p) = save_snapshot {
        anyhow::ensure!(!insecure, "снимок без проверки сертификата SQL Server не сохраняется");
        let file = SnapshotFile { source, snapshot };
        std::fs::write(&p, serde_json::to_vec(&file)?).with_context(|| p.display().to_string())?;
        let (_, report) = file.snapshot.plan();
        say(&serde_json::to_string_pretty(&report)?);
        say(&format!("Снимок сохранён: {} — в нём персональные данные, удалите его после переноса.", p.display()));
        return Ok(());
    }
    let db = db_pool(cfg, false).await?;
    let core = core_state(cfg, db)?;
    let actor = pult_core::audit::Actor::system();
    let outcome = pult_phonebook::import::apply(&core.db, &snapshot, dry_run, &actor, &source).await?;
    say(&serde_json::to_string_pretty(&outcome)?);
    if !dry_run {
        // Связать перенесённых сотрудников с учётками AD (если синхронизация уже была).
        let linked = pult_people::sync::link_all(&core).await?;
        say(&format!(
            "Связано с AD: {}, на подтверждение: {}, создано из AD: {}",
            linked.linked, linked.candidates, linked.persons_created
        ));
        // Редакторы Phonebook — однократно «Редактор справочника».
        let ad = pult_people::domain::AdHandle::default();
        match pult_people::sync::grant_role_to_group_once(
            &core,
            &ad,
            "PhonebookAdmins",
            pult_core::rbac::ROLE_PHONEBOOK_EDITOR,
        )
        .await
        {
            Ok(Some(n)) => say(&format!("Роль «Редактор справочника» выдана участникам PhonebookAdmins: {n}")),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "перенос роли PhonebookAdmins не удался — выдайте её в админке"),
        }
    }
    say(if outcome.reconciliation.matches {
        "Сверка: совпадает с Phonebook."
    } else {
        "Сверка: НЕ совпадает с Phonebook — смотрите reconciliation."
    });
    Ok(())
}

async fn setup_ad(
    cfg: &Config,
    base_dn: String,
    controllers: Vec<String>,
    bind_dn: String,
    netbios: Option<String>,
    tls: String,
    ca_cert: Option<PathBuf>,
) -> anyhow::Result<()> {
    let tls = match tls.as_str() {
        "ldaps" => pult_ad::TlsMode::Ldaps,
        "starttls" => pult_ad::TlsMode::Starttls,
        other => anyhow::bail!("--tls: ldaps или starttls, а не {other}"),
    };
    let password = match std::env::var("PULT_AD_BIND_PASSWORD") {
        Ok(p) if !p.is_empty() => Secret::new(p),
        _ => Secret::new(rpassword::prompt_password(format!("Пароль {bind_dn}: "))?),
    };
    let ad = pult_ad::AdConfig {
        controllers,
        tls,
        base_dn,
        bind_dn,
        bind_password: password,
        ca_cert,
        ca_cert_pem: None,
        user_filter: "(&(objectCategory=person)(objectClass=user))".into(),
        timeout_secs: 5,
    };
    // Сначала проверка: с неверным паролем или без доступного DC домен не сохраняем.
    let pool = DcPool::new(ad.clone())?;
    let checks = pool.check_controllers().await;
    for c in &checks {
        say(&format!(
            "{} {}{}",
            if c.ok { "✓" } else { "✗" },
            c.controller,
            c.error.as_deref().map(|e| format!(" — {e}")).unwrap_or_default()
        ));
        if let Some(h) = &c.hint {
            say(&format!("  → {h}"));
        }
    }
    anyhow::ensure!(checks.iter().any(|c| c.ok), "ни один контроллер не принял служебную учётку — домен не сохранён");
    let core = core_state(cfg, db_pool(cfg, false).await?)?;
    pult_people::domain::store(&core, &ad, netbios.as_deref(), true).await?;
    pult_core::audit::record(
        &core.db,
        &pult_core::audit::Actor::system(),
        pult_core::audit::Entry::new("ad.domain.update", pult_core::audit::Source::Cli)
            .details(serde_json::json!({"base_dn": ad.base_dn, "controllers": ad.controllers, "bind_dn": ad.bind_dn})),
    )
    .await?;
    say("Домен сохранён. Синхронизация пойдёт по расписанию (каждые 5 минут) или по кнопке в админке.");
    Ok(())
}

async fn check_ad(cfg: &Config, login: Option<String>, since_usn: Option<u64>, page_size: i32) -> anyhow::Result<()> {
    let pool = DcPool::new(cfg.ad()?.clone())?;
    let dcs: Vec<String> = pool.endpoints().iter().map(|e| e.to_string()).collect();
    say(&format!("Контроллеры: {}", dcs.join(", ")));
    if let Some(login) = login {
        // Для CI и стенда — из переменной; в терминале — запрос без отображения.
        let password = match std::env::var("PULT_CHECK_AD_PASSWORD") {
            Ok(p) if !p.is_empty() => Secret::new(p),
            _ => Secret::new(rpassword::prompt_password(format!("Пароль для {login}: "))?),
        };
        match pool.authenticate(&login, &password).await {
            Ok(u) => {
                say(&format!("Вход выполнен: {} ({}) через {}", u.display_name.unwrap_or_default(), u.dn, u.controller))
            }
            Err(AdError::Bind(reason)) => say(&format!("Вход отклонён: {} [{reason:?}]", reason.user_message())),
            Err(e) => return Err(e.into()),
        }
        return Ok(());
    }
    let batch = match since_usn {
        Some(usn) => pool.incremental_sync(usn, page_size).await?,
        None => pool.full_sync(page_size).await?,
    };
    let disabled = batch.accounts.iter().filter(|a| !a.is_enabled).count();
    say(&format!(
        "{} синхронизация через {}: учёток {}, отключённых {}, highestCommittedUSN {}",
        if since_usn.is_some() { "Инкрементальная" } else { "Полная" },
        batch.controller,
        batch.accounts.len(),
        disabled,
        batch.highest_usn
    ));
    Ok(())
}

/// Уведомления на устройства уходят сразу по событию «уведомление», а не ждут минутной задачи.
/// Рассылка забирает уведомления атомарно, так что несколько копий сервера не пришлют одно дважды.
fn spawn_push(core: pult_core::Core) {
    let mut rx = core.events.subscribe();
    let mut stopped = core.events.stopped();
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                e = rx.recv() => e,
                _ = stopped.wait_for(|s| *s) => return,
            };
            match event {
                Ok(e) if e.kind == "notification" => {
                    if let Err(e) = pult_people::push::deliver(&core).await {
                        tracing::warn!(error = %e, "рассылка уведомлений на устройства");
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}
