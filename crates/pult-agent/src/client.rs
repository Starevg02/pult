//! HTTPS-клиент портала: регистрация, отчёт, скачивание обновления.
//!
//! Только HTTPS. Исключение — адреса этой же машины (`localhost`, `127.0.0.1`, `::1`)
//! для разработки. Проверку сертификата сервера отключить нельзя. Токен и ключ — в заголовке
//! или теле, не в адресе (адреса попадают в журналы прокси).

use pult_api_types::{AgentReport, EnrollRequest, EnrollResponse, ReportResponse};
use pult_core::Secret;
use std::time::Duration;
use url::Url;

/// Наибольший пакет обновления.
pub const MAX_UPDATE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    #[error("неверный адрес портала: {0}")]
    BadUrl(String),
    #[error("адрес портала должен быть https:// (http допускается только для localhost)")]
    NotHttps,
    #[error("портал не принял токен или ключ агента (401)")]
    Unauthorized,
    #[error("имя ПК изменилось — нужна новая регистрация (409)")]
    Renamed,
    #[error("портал отказал: HTTP {0}: {1}")]
    Rejected(u16, String),
    #[error("нет связи с порталом: {0}")]
    Network(String),
}

/// Адрес портала, проверенный на HTTPS.
pub fn base_url(portal: &str) -> Result<Url, PortalError> {
    let mut base = Url::parse(portal.trim()).map_err(|e| PortalError::BadUrl(e.to_string()))?;
    let local = matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"));
    match base.scheme() {
        "https" => {}
        "http" if local => {}
        _ => return Err(PortalError::NotHttps),
    }
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    Ok(base)
}

pub struct Portal {
    base: Url,
    http: reqwest::Client,
}

impl Portal {
    pub fn new(portal: &str) -> Result<Self, PortalError> {
        let base = base_url(portal)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("pult-agent/", env!("CARGO_PKG_VERSION")))
            .https_only(base.scheme() == "https")
            .build()
            .map_err(|e| PortalError::Network(e.to_string()))?;
        Ok(Self { base, http })
    }

    pub fn url(&self, path: &str) -> Result<Url, PortalError> {
        self.base.join(path).map_err(|e| PortalError::BadUrl(e.to_string()))
    }

    /// Хост и порт портала — чтобы узнать, с какого адреса ПК до него ходит.
    pub fn host_port(&self) -> Option<(String, u16)> {
        Some((self.base.host_str()?.to_string(), self.base.port_or_known_default()?))
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response, PortalError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        Err(match status.as_u16() {
            401 => PortalError::Unauthorized,
            409 => PortalError::Renamed,
            code => {
                let text: String = resp.text().await.unwrap_or_default().chars().take(300).collect();
                PortalError::Rejected(code, text)
            }
        })
    }

    pub async fn enroll(&self, token: &Secret, report: &AgentReport) -> Result<EnrollResponse, PortalError> {
        let body = EnrollRequest { token: token.expose().to_string(), report: report.clone() };
        let resp = self
            .http
            .post(self.url("api/agent/v1/enroll")?)
            .json(&body)
            .send()
            .await
            .map_err(|e| PortalError::Network(e.without_url().to_string()))?;
        Self::check(resp).await?.json().await.map_err(|e| PortalError::Network(e.without_url().to_string()))
    }

    pub async fn report(&self, key: &Secret, report: &AgentReport) -> Result<ReportResponse, PortalError> {
        let resp = self
            .http
            .post(self.url("api/agent/v1/report")?)
            .bearer_auth(key.expose())
            .json(report)
            .send()
            .await
            .map_err(|e| PortalError::Network(e.without_url().to_string()))?;
        Self::check(resp).await?.json().await.map_err(|e| PortalError::Network(e.without_url().to_string()))
    }

    pub async fn download_update(&self, key: &Secret, package_id: &str) -> Result<Vec<u8>, PortalError> {
        if !package_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') || package_id.len() > 40 {
            return Err(PortalError::BadUrl("package id".into()));
        }
        let mut resp = Self::check(
            self.http
                .get(self.url(&format!("api/agent/v1/update/{package_id}"))?)
                .bearer_auth(key.expose())
                .timeout(Duration::from_secs(300))
                .send()
                .await
                .map_err(|e| PortalError::Network(e.without_url().to_string()))?,
        )
        .await?;
        let mut out = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| PortalError::Network(e.without_url().to_string()))? {
            if out.len() + chunk.len() > MAX_UPDATE_BYTES {
                return Err(PortalError::Rejected(413, "пакет больше 64 МБ".into()));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_required_except_localhost() {
        assert!(base_url("https://pult.example.local/").is_ok());
        assert!(base_url("http://127.0.0.1:8080/").is_ok());
        assert!(base_url("http://localhost:8080").is_ok());
        assert!(matches!(base_url("http://pult.example.local/"), Err(PortalError::NotHttps)));
        assert!(matches!(base_url("ftp://pult/"), Err(PortalError::NotHttps)));
        assert!(matches!(base_url("не адрес"), Err(PortalError::BadUrl(_))));
    }

    #[test]
    fn endpoints() {
        let p = Portal::new("https://pult.example.local").unwrap();
        assert_eq!(p.url("api/agent/v1/report").unwrap().as_str(), "https://pult.example.local/api/agent/v1/report");
        let p = Portal::new("https://pult.example.local/pult").unwrap();
        assert_eq!(
            p.url("api/agent/v1/enroll").unwrap().as_str(),
            "https://pult.example.local/pult/api/agent/v1/enroll"
        );
        assert_eq!(p.host_port(), Some(("pult.example.local".into(), 443)));
    }
}
