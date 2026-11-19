//! Журналы (`tracing`) и метрики (Prometheus).

use crate::config::LogFormat;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Журнал в stderr. Уровень — `RUST_LOG` (по умолчанию `info`).
pub fn init_tracing(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
    let registry = tracing_subscriber::registry().with(filter);
    let res = match format {
        LogFormat::Json => registry.with(fmt::layer().json().with_writer(std::io::stderr)).try_init(),
        LogFormat::Text => registry.with(fmt::layer().with_ansi(use_color()).with_writer(std::io::stderr)).try_init(),
    };
    // Повторная инициализация (тесты) не ошибка.
    let _ = res;
}

/// Цвет — только в терминале, который его понимает. Windows PowerShell 5.1 (conhost) показывает
/// коды цвета как `←[33m`; Windows Terminal выставляет `WT_SESSION`, VS Code и прочие — `TERM`.
fn use_color() -> bool {
    use std::io::IsTerminal;
    let var = |k| std::env::var_os(k).is_some();
    color_allowed(std::io::stderr().is_terminal(), var("NO_COLOR"), cfg!(windows), var("WT_SESSION") || var("TERM"))
}

fn color_allowed(is_terminal: bool, no_color: bool, windows: bool, modern_windows_terminal: bool) -> bool {
    is_terminal && !no_color && (!windows || modern_windows_terminal)
}

/// Глобальный регистратор метрик ставится один раз на процесс.
pub fn metrics_handle() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let recorder = PrometheusBuilder::new().build_recorder();
            let handle = recorder.handle();
            // Если регистратор уже установлен кем-то ещё — метрики просто не будут собираться.
            let _ = metrics::set_global_recorder(recorder);
            handle
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::color_allowed;

    #[test]
    fn color_only_where_it_renders() {
        assert!(color_allowed(true, false, false, false), "Linux-терминал");
        assert!(!color_allowed(true, false, true, false), "Windows PowerShell 5.1");
        assert!(color_allowed(true, false, true, true), "Windows Terminal");
        assert!(!color_allowed(true, true, false, false), "NO_COLOR");
        assert!(!color_allowed(false, false, false, false), "вывод в файл или журнал");
    }
}
