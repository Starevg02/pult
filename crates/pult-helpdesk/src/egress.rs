//! Исходящие запросы по адресам, которые вводит администратор (сервер модели, NetBox, вебхуки, манифесты
//! расширений). Внутренние серверы — их назначение, поэтому частные сети и localhost разрешены; закрыты
//! служебные адреса: link-local (метаданные облака 169.254.169.254, fe80::/10) и «любой адрес».
//!
//! Проверки две: адрес-число в URL — сразу при сохранении, имя — при каждом запросе, после разрешения DNS
//! (имя могло указывать на служебный адрес с самого начала или начать указывать позже).

use std::{
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Метаданные AWS по IPv6.
const AWS_METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// Служебный адрес, куда портал не ходит. IPv4, записанный внутри IPv6 (`::ffff:169.254.169.254`), проверяется
/// как IPv4.
pub(crate) fn forbidden(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80 || v6.is_unspecified() || v6 == AWS_METADATA_V6,
    }
}

/// Адрес-число в URL — служебный. Имена проверяет [`client`] при запросе.
/// URL уже приводит адрес-число к обычной записи (`2852039166` → `169.254.169.254`).
pub(crate) fn literal_forbidden(u: &reqwest::Url) -> bool {
    let host = u.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
    host.parse::<IpAddr>().is_ok_and(forbidden)
}

/// Разрешение имён без служебных адресов: если имя указывает только на них, запроса не будет.
struct Guard;

impl Resolve for Guard {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let found: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed: Vec<SocketAddr> = found.into_iter().filter(|a| !forbidden(a.ip())).collect();
            if allowed.is_empty() {
                return Err(format!("{host}: link-local address is not allowed").into());
            }
            let addrs: Addrs = Box::new(allowed.into_iter());
            Ok(addrs)
        })
    }
}

/// Клиент для адресов, введённых администратором: без служебных адресов и без перенаправлений.
pub(crate) fn client() -> reqwest::ClientBuilder {
    reqwest::Client::builder().dns_resolver(Arc::new(Guard)).redirect(reqwest::redirect::Policy::none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_addresses() {
        for bad in ["169.254.169.254", "::ffff:169.254.169.254", "fe80::1", "0.0.0.0", "::", "fd00:ec2::254"] {
            assert!(forbidden(bad.parse().unwrap()), "{bad}");
        }
        for ok in ["10.0.0.5", "192.168.1.1", "127.0.0.1", "::1", "8.8.8.8", "2001:db8::1"] {
            assert!(!forbidden(ok.parse().unwrap()), "{ok}");
        }
        let u = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(literal_forbidden(&u("http://[::ffff:169.254.169.254]/v1")));
        assert!(literal_forbidden(&u("http://2852039166/")), "169.254.169.254 десятичным числом");
        assert!(!literal_forbidden(&u("http://ai.local:8080/v1")));
    }

    #[tokio::test]
    async fn names_resolve_without_service_addresses() {
        let ok = Guard.resolve("localhost".parse().unwrap()).await;
        assert!(ok.is_ok_and(|mut a| a.next().is_some()));
    }
}
