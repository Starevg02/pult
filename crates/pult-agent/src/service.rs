//! Служба Windows: SCM запускает `pult-agent.exe service`; остановка и события сессий
//! (вход, выход, блокировка, отключение) приходят через обработчик управления.

use crate::{SERVICE_NAME, run};
use std::{ffi::OsString, sync::Arc, time::Duration};
use tokio::sync::{Notify, watch};
use windows_service::{
    define_windows_service,
    service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType},
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

define_windows_service!(ffi_service_main, service_main);

/// Передать управление SCM (возвращается, когда служба остановлена).
pub fn start() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!(error = %format!("{e:#}"), "служба агента завершилась с ошибкой");
        // Выход без «служба остановлена», как при самообновлении: SCM считает это сбоем
        // и перезапускает службу по настройке восстановления (MSI).
        std::process::exit(1);
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    }
}

fn run_service() -> anyhow::Result<()> {
    let wake = Arc::new(Notify::new());
    let (stop_tx, stop_rx) = watch::channel(false);
    let w = wake.clone();
    let handler = move |c: ServiceControl| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(_) => {
            w.notify_one();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle = service_control_handler::register(SERVICE_NAME, handler)?;
    handle.set_service_status(status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN | ServiceControlAccept::SESSION_CHANGE,
    ))?;
    let result = (|| -> anyhow::Result<()> {
        let agent = crate::windows_agent()?;
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?
            .block_on(run::run(agent, wake, stop_rx));
        Ok(())
    })();
    result?;
    handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}
