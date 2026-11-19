//! Встроенный HTTPS (TLS встроенный или за обратным прокси — оба режима поддерживаются).
//!
//! Сертификат — цепочка PEM (сертификат сервера + промежуточные), ключ — PEM (PKCS#8, SEC1 или PKCS#1).
//! Криптография — `ring`, как и у клиента AD; TLS 1.2 и 1.3, ALPN `h2` и `http/1.1`.

use axum_server::tls_rustls::RustlsConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use x509_parser::{extensions::GeneralName, pem::parse_x509_pem};

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("сертификат {path}: {reason}")]
    Cert { path: String, reason: String },
    #[error("ключ {path}: {reason}")]
    Key { path: String, reason: String },
    #[error("сертификат и ключ не подходят друг другу или не поддерживаются: {0}")]
    Mismatch(String),
    #[error("{0}")]
    Invalid(String),
    #[error("каталог сертификата {path}: {source}")]
    Io { path: String, source: std::io::Error },
}

pub fn server_config(cert: &Path, key: &Path) -> Result<rustls::ServerConfig, TlsError> {
    server_config_with(cert, key, None)
}

/// Основная пара и, по желанию, вторая — для её собственных имён. Клиент, запросивший (SNI) имя из
/// второго сертификата, получает его; остальные, в том числе без SNI и по IP, — основной. Так у портала
/// может быть внутреннее имя от своего CA и внешнее — от публичного.
pub fn server_config_with(
    cert: &Path,
    key: &Path,
    extra: Option<(&Path, &Path)>,
) -> Result<rustls::ServerConfig, TlsError> {
    let (chain, key) = read_pair(cert, key)?;
    let Some((extra_cert, extra_key)) = extra else {
        return build(chain, key);
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let default = Arc::new(certified(chain, key, &provider)?);
    let pem = std::fs::read(extra_cert)
        .map_err(|e| TlsError::Cert { path: extra_cert.display().to_string(), reason: e.to_string() })?;
    let names: Vec<String> = inspect(&pem, false)?
        .names
        .into_iter()
        .filter(|n| n.parse::<std::net::IpAddr>().is_err())
        .map(|n| n.to_ascii_lowercase())
        .collect();
    if names.is_empty() {
        return Err(TlsError::Cert { path: extra_cert.display().to_string(), reason: "нет имён DNS".into() });
    }
    let (chain, key) = read_pair(extra_cert, extra_key)?;
    let extra = Arc::new(certified(chain, key, &provider)?);
    let mut cfg = builder(provider)?.with_cert_resolver(Arc::new(BySni { default, extra, names }));
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

type Pair = (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>);

fn read_pair(cert: &Path, key: &Path) -> Result<Pair, TlsError> {
    let cert_err = |reason: String| TlsError::Cert { path: cert.display().to_string(), reason };
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .and_then(|it| it.collect::<Result<_, _>>())
        .map_err(|e| cert_err(e.to_string()))?;
    if chain.is_empty() {
        return Err(cert_err("нет сертификатов в PEM".into()));
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| TlsError::Key { path: key.display().to_string(), reason: e.to_string() })?;
    Ok((chain, key))
}

fn certified(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    provider: &rustls::crypto::CryptoProvider,
) -> Result<rustls::sign::CertifiedKey, TlsError> {
    rustls::sign::CertifiedKey::from_der(chain, key, provider).map_err(|e| TlsError::Mismatch(e.to_string()))
}

fn builder(
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<rustls::ConfigBuilder<rustls::ServerConfig, rustls::server::WantsServerCert>, TlsError> {
    Ok(rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Mismatch(e.to_string()))?
        .with_no_client_auth())
}

fn build(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<rustls::ServerConfig, TlsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = builder(provider)?.with_single_cert(chain, key).map_err(|e| TlsError::Mismatch(e.to_string()))?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

/// Выбор сертификата по запрошенному имени.
#[derive(Debug)]
struct BySni {
    default: Arc<rustls::sign::CertifiedKey>,
    extra: Arc<rustls::sign::CertifiedKey>,
    /// Имена DNS второго сертификата, в нижнем регистре; `*.` — один уровень слева.
    names: Vec<String>,
}

impl rustls::server::ResolvesServerCert for BySni {
    fn resolve(&self, hello: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let wanted = hello.server_name().is_some_and(|host| self.names.iter().any(|p| name_matches(p, host)));
        Some(if wanted { self.extra.clone() } else { self.default.clone() })
    }
}

/// Имя из сертификата (`pattern`, в нижнем регистре) подходит к запрошенному: точно или по `*.` на один уровень.
fn name_matches(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.split_once('.').is_some_and(|(first, rest)| !first.is_empty() && rest == suffix),
        None => host == pattern,
    }
}

/// Сведения о сертификате для админки.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct TlsInfo {
    pub subject: String,
    pub issuer: String,
    /// Срок действия, RFC 3339.
    pub not_after: String,
    /// Имена и адреса из сертификата.
    pub names: Vec<String>,
    /// Самоподписанный: браузеры предупреждают, стоит заменить.
    pub self_signed: bool,
}

/// Разбирает первый сертификат PEM. Заодно отвергает просроченный и ещё не действующий.
fn inspect(pem: &[u8], require_valid: bool) -> Result<TlsInfo, TlsError> {
    let bad = |m: String| TlsError::Invalid(m);
    let (_, p) = parse_x509_pem(pem).map_err(|_| bad("Сертификат — не в формате PEM.".into()))?;
    let c = p.parse_x509().map_err(|e| bad(format!("Не удалось разобрать сертификат: {e}")))?;
    if require_valid && !c.validity().is_valid() {
        return Err(bad("Сертификат просрочен или ещё не начал действовать.".into()));
    }
    let mut names = Vec::new();
    if let Ok(Some(san)) = c.subject_alternative_name() {
        for n in &san.value.general_names {
            match n {
                GeneralName::DNSName(d) => names.push((*d).to_string()),
                GeneralName::IPAddress(b) => {
                    if let Ok(a) = <[u8; 4]>::try_from(*b) {
                        names.push(std::net::IpAddr::from(a).to_string());
                    } else if let Ok(a) = <[u8; 16]>::try_from(*b) {
                        names.push(std::net::IpAddr::from(a).to_string());
                    }
                }
                _ => {}
            }
        }
    }
    let not_after = time::OffsetDateTime::from_unix_timestamp(c.validity().not_after.timestamp())
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default();
    Ok(TlsInfo {
        subject: c.subject().to_string(),
        issuer: c.issuer().to_string(),
        not_after,
        names,
        self_signed: c.subject() == c.issuer(),
    })
}

/// Временный файл рядом с `path` с данными целиком (только для владельца).
fn write_tmp(path: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = crate::config::create_private(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(tmp)
}

/// Ключ и сертификат: сначала оба временных файла, потом два переименования подряд. Ошибка записи
/// (место, права) не оставляет на диске ключ от одной пары и сертификат от другой.
fn write_pair(key: &Path, key_pem: &[u8], cert: &Path, cert_pem: &[u8]) -> std::io::Result<()> {
    let key_tmp = write_tmp(key, key_pem)?;
    let cert_tmp = match write_tmp(cert, cert_pem) {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_file(&key_tmp);
            return Err(e);
        }
    };
    std::fs::rename(key_tmp, key)?;
    std::fs::rename(cert_tmp, cert)
}

/// Сертификат, которым управляет сам портал: лежит в каталоге данных, заменяется без перезапуска.
#[derive(Clone)]
pub struct ManagedTls {
    dir: PathBuf,
    pub rustls: RustlsConfig,
    replacing: Arc<tokio::sync::Mutex<()>>,
}

impl ManagedTls {
    /// Открывает сертификат из каталога; при первом запуске создаёт временный самоподписанный.
    pub fn open(dir: &Path, hosts: Vec<String>) -> Result<Self, TlsError> {
        let io = |source| TlsError::Io { path: dir.display().to_string(), source };
        std::fs::create_dir_all(dir).map_err(io)?;
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        if !cert.exists() || !key.exists() {
            let (c, k) = self_signed(hosts)?;
            write_pair(&key, k.as_bytes(), &cert, c.as_bytes()).map_err(io)?;
            tracing::warn!("создан временный самоподписанный сертификат — браузер предупредит; замените его в админке");
        }
        let rustls = RustlsConfig::from_config(Arc::new(server_config(&cert, &key)?));
        Ok(Self { dir: dir.to_path_buf(), rustls, replacing: Arc::default() })
    }

    pub fn info(&self) -> Result<TlsInfo, TlsError> {
        let path = self.dir.join("cert.pem");
        let pem = std::fs::read(&path).map_err(|source| TlsError::Io { path: path.display().to_string(), source })?;
        inspect(&pem, false)
    }

    /// Проверяет пару, сохраняет и применяет сразу. С неподходящей парой ничего не меняется.
    pub async fn replace(&self, cert_pem: &[u8], key_pem: &[u8]) -> Result<TlsInfo, TlsError> {
        let info = inspect(cert_pem, true)?;
        let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<Result<_, _>>()
            .map_err(|e| TlsError::Invalid(format!("Сертификат: {e}")))?;
        let key = PrivateKeyDer::from_pem_slice(key_pem)
            .map_err(|_| TlsError::Invalid("Закрытый ключ — не в формате PEM или зашифрован паролем.".into()))?;
        let cfg = build(chain, key)?;
        let _one_at_a_time = self.replacing.lock().await;
        let io = |source| TlsError::Io { path: self.dir.display().to_string(), source };
        write_pair(&self.dir.join("key.pem"), key_pem, &self.dir.join("cert.pem"), cert_pem).map_err(io)?;
        self.rustls.reload_from_config(Arc::new(cfg));
        Ok(info)
    }
}

fn self_signed(hosts: Vec<String>) -> Result<(String, String), TlsError> {
    let bad = |e: rcgen::Error| TlsError::Invalid(e.to_string());
    let mut p = rcgen::CertificateParams::new(hosts).map_err(bad)?;
    p.distinguished_name.push(rcgen::DnType::CommonName, "Pult (временный сертификат)");
    let now = time::OffsetDateTime::now_utc();
    p.not_before = now - time::Duration::days(1);
    p.not_after = now + time::Duration::days(825);
    let key = rcgen::KeyPair::generate().map_err(bad)?;
    let cert = p.self_signed(&key).map_err(bad)?;
    Ok((cert.pem(), key.serialize_pem()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pult-tls-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pair(d: &Path, tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["pult.example.test".into()]).unwrap().self_signed(&key).unwrap();
        let (c, k) = (d.join(format!("{tag}.crt")), d.join(format!("{tag}.key")));
        std::fs::write(&c, cert.pem()).unwrap();
        std::fs::write(&k, key.serialize_pem()).unwrap();
        (c, k)
    }

    #[test]
    fn loads_matching_pair_with_alpn() {
        let d = tmp("ok");
        let (c, k) = pair(&d, "a");
        let cfg = server_config(&c, &k).unwrap();
        assert_eq!(cfg.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn managed_certificate_is_created_kept_and_replaced() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let d = tmp("managed");
        let m = ManagedTls::open(&d, vec!["pult.example.test".into(), "192.0.2.10".into()]).unwrap();
        let first = m.info().unwrap();
        assert!(first.self_signed, "{first:?}");
        assert_eq!(first.names, ["pult.example.test", "192.0.2.10"]);
        // Повторный запуск не пересоздаёт сертификат.
        let again = ManagedTls::open(&d, vec!["other.example.test".into()]).unwrap().info().unwrap();
        assert_eq!(again.names, first.names);

        let (c1, k1) = self_signed(vec!["new.example.test".into()]).unwrap();
        let (_, k2) = self_signed(vec!["x.example.test".into()]).unwrap();
        let chain = |pem: &str| pem.as_bytes().to_vec();
        assert!(matches!(m.replace(&chain(&c1), k2.as_bytes()).await, Err(TlsError::Mismatch(_))), "чужой ключ");
        assert!(m.replace(b"garbage", k1.as_bytes()).await.is_err());
        assert!(m.replace(&chain(&c1), b"garbage").await.is_err());
        assert_eq!(m.info().unwrap().names, first.names, "неверная пара ничего не меняет");
        m.replace(&chain(&c1), k1.as_bytes()).await.unwrap();
        assert_eq!(m.info().unwrap().names, ["new.example.test"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn wildcard_and_exact_names_match_one_level() {
        assert!(name_matches("*.example.com", "pult.example.com"));
        assert!(name_matches("*.example.com", "PULT.Example.COM."));
        assert!(!name_matches("*.example.com", "example.com"));
        assert!(!name_matches("*.example.com", "a.b.example.com"));
        assert!(!name_matches("*.example.com", ".example.com"));
        assert!(name_matches("pult.example.com", "pult.example.com"));
        assert!(!name_matches("pult.example.com", "mail.example.com"));
    }

    /// Рукопожатие в памяти: какой сертификат сервер отдал клиенту, запросившему `sni`.
    fn served_names(cfg: rustls::ServerConfig, sni: &str, roots: &[&Path]) -> Vec<String> {
        let mut store = rustls::RootCertStore::empty();
        for r in roots {
            for c in CertificateDer::pem_file_iter(r).unwrap() {
                store.add(c.unwrap()).unwrap();
            }
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let client_cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(store)
            .with_no_client_auth();
        let name = rustls_pki_types::ServerName::try_from(sni.to_string()).unwrap();
        let mut client = rustls::ClientConnection::new(Arc::new(client_cfg), name).unwrap();
        let mut server = rustls::ServerConnection::new(Arc::new(cfg)).unwrap();
        for _ in 0..10 {
            let mut buf = Vec::new();
            client.write_tls(&mut buf).unwrap();
            server.read_tls(&mut buf.as_slice()).unwrap();
            server.process_new_packets().unwrap();
            let mut buf = Vec::new();
            server.write_tls(&mut buf).unwrap();
            client.read_tls(&mut buf.as_slice()).unwrap();
            client.process_new_packets().unwrap();
            if !client.is_handshaking() && !server.is_handshaking() {
                break;
            }
        }
        assert!(!client.is_handshaking(), "рукопожатие не завершилось");
        let leaf = &client.peer_certificates().unwrap()[0];
        let (_, c) = x509_parser::parse_x509_certificate(leaf).unwrap();
        let mut names = Vec::new();
        if let Ok(Some(san)) = c.subject_alternative_name() {
            for n in &san.value.general_names {
                if let GeneralName::DNSName(d) = n {
                    names.push((*d).to_string());
                }
            }
        }
        names
    }

    fn named_pair(d: &Path, tag: &str, names: &[&str]) -> (std::path::PathBuf, std::path::PathBuf) {
        let key = rcgen::KeyPair::generate().unwrap();
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let cert = rcgen::CertificateParams::new(names).unwrap().self_signed(&key).unwrap();
        let (c, k) = (d.join(format!("{tag}.crt")), d.join(format!("{tag}.key")));
        std::fs::write(&c, cert.pem()).unwrap();
        std::fs::write(&k, key.serialize_pem()).unwrap();
        (c, k)
    }

    #[test]
    fn second_certificate_is_served_for_its_names_only() {
        let d = tmp("sni");
        let (c1, k1) = named_pair(&d, "inner", &["pult.corp.test"]);
        let (c2, k2) = named_pair(&d, "outer", &["*.example.com"]);
        let roots = [c1.as_path(), c2.as_path()];
        let cfg = || server_config_with(&c1, &k1, Some((&c2, &k2))).unwrap();
        assert_eq!(cfg().alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
        assert_eq!(served_names(cfg(), "pult.example.com", &roots), ["*.example.com"]);
        assert_eq!(served_names(cfg(), "pult.corp.test", &roots), ["pult.corp.test"]);
        // Чужой ключ у второй пары и сертификат без имён DNS — сервер не стартует.
        assert!(matches!(server_config_with(&c1, &k1, Some((&c2, &k1))), Err(TlsError::Mismatch(_))));
        let key = rcgen::KeyPair::generate().unwrap();
        let ip_only = rcgen::CertificateParams::new(vec!["192.0.2.10".to_string()]).unwrap().self_signed(&key).unwrap();
        std::fs::write(d.join("ip.crt"), ip_only.pem()).unwrap();
        std::fs::write(d.join("ip.key"), key.serialize_pem()).unwrap();
        assert!(matches!(
            server_config_with(&c1, &k1, Some((&d.join("ip.crt"), &d.join("ip.key")))),
            Err(TlsError::Cert { .. })
        ));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn expired_certificate_is_rejected() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut p = rcgen::CertificateParams::new(vec!["old.example.test".to_string()]).unwrap();
        p.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(30);
        p.not_after = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        let pem = p.self_signed(&key).unwrap().pem();
        assert!(inspect(pem.as_bytes(), true).is_err());
        assert!(inspect(pem.as_bytes(), false).is_ok(), "текущий просроченный — показать в админке можно");
    }

    #[test]
    fn rejects_foreign_key_missing_files_and_garbage() {
        let d = tmp("bad");
        let (c1, _) = pair(&d, "a");
        let (_, k2) = pair(&d, "b");
        assert!(matches!(server_config(&c1, &k2), Err(TlsError::Mismatch(_))), "чужой ключ");
        assert!(matches!(server_config(&d.join("nope.crt"), &k2), Err(TlsError::Cert { .. })));
        assert!(matches!(server_config(&c1, &d.join("nope.key")), Err(TlsError::Key { .. })));
        std::fs::write(d.join("empty.crt"), "не PEM").unwrap();
        assert!(matches!(server_config(&d.join("empty.crt"), &k2), Err(TlsError::Cert { .. })));
        let _ = std::fs::remove_dir_all(d);
    }
}
