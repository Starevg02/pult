#!/bin/bash
# Сквозная проверка установки на одноразовой машине Debian (systemd, PostgreSQL, root): чистая установка пакета,
# мастер первого запуска на поддельном контроллере из `pult demo`, настройка организации и первая
# заявка — с замером времени. После себя удаляет пакет, пользователя pult и базы pult/pult_demo.
#
#   deploy/ci/deb-wizard-e2e.sh pult_<версия>_amd64.deb      (из корня репозитория, нужен cargo)
#
# Настоящий контроллер вместо поддельного: PULT_E2E_DC=dc.pult.test:636 PULT_E2E_CA=ca.pem и пароли
# E2E_SVC_PASS (svc-pult@pult.test) и E2E_USER_PASS (admin) в окружении.
#
# ВНИМАНИЕ: пересоздаёт базу pult в локальном PostgreSQL. Только для стенда CI.
set -uo pipefail
DEB=${1:?пакет .deb}
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE/../.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
LOG=/tmp/pult-e2e-demo.log
DEMO_PID=""

cleanup() {
    [ -n "$DEMO_PID" ] && kill "$DEMO_PID" 2>/dev/null
    pkill -f "target/debug/pult demo" 2>/dev/null
    dpkg --purge pult >/dev/null 2>&1 || true
    rm -rf /etc/pult /var/lib/pult /var/lib/pult-rollback /tmp/pult-demo-data /tmp/pult-e2e-ca.pem
    deluser --quiet --system pult >/dev/null 2>&1 || true
    # Роль pult переживает пакет: вернуть пароль CI (задаётся снаружи, по умолчанию — как в CI).
    runuser -u postgres -- psql -XqAt -c "ALTER ROLE pult PASSWORD '${PULT_CI_DB_PASSWORD:-pult-ci}'" >/dev/null 2>&1 || true
    ( cd /tmp && runuser -u postgres -- dropdb --if-exists --force pult_demo
      runuser -u postgres -- dropdb --if-exists --force pult && runuser -u postgres -- createdb -O pult pult ) || true
}
# Код выхода — сценария, а не уборки.
trap 'rc=$?; cleanup; exit $rc' EXIT
# Установка должна быть чистой: прежний пакет, его настройки и данные — убрать до начала.
cleanup

q() { runuser -u postgres -- psql -XtAq -d "$1" -c "$2"; }
if [ -n "${PULT_E2E_DC:-}" ]; then
    # Настоящий контроллер (например, тестовый Samba AD): адрес, CA и пароли — снаружи.
    DC=$PULT_E2E_DC
    cp "${PULT_E2E_CA:?корневой сертификат контроллера}" /tmp/pult-e2e-ca.pem
    : "${E2E_SVC_PASS:?пароль svc-pult}" "${E2E_USER_PASS:?пароль admin}"
    echo "domain controller $DC"
else
    echo "=== demo: fake domain controller"
    cargo build -q -p pult-server --features demo || exit 1
    ( cd /tmp && runuser -u postgres -- dropdb --if-exists --force pult_demo && runuser -u postgres -- createdb -O pult pult_demo ) || exit 1
    mkdir -p /tmp/pult-demo-data
    PULT_DATABASE_URL=postgres://pult:${PULT_CI_DB_PASSWORD:-pult-ci}@localhost:5432/pult_demo PULT_LISTEN=127.0.0.1:8080 PULT_DATA_DIR=/tmp/pult-demo-data         target/debug/pult demo >"$LOG" 2>&1 &
    DEMO_PID=$!
    for i in $(seq 1 60); do grep -q "синхронизация AD завершена" "$LOG" && break; sleep 1; done
    grep -q "синхронизация AD завершена" "$LOG" || { tail -20 "$LOG"; echo "=== FAIL demo"; exit 1; }
    DC=$(cd /tmp && q pult_demo "SELECT controllers[1] FROM ad_domains")
    (cd /tmp && q pult_demo "SELECT ca_cert_pem FROM ad_domains") >/tmp/pult-e2e-ca.pem
    E2E_SVC_PASS=$(grep -o 'SVC_PASS: &str = "[^"]*"' crates/pult-testkit/src/lib.rs | sed 's/.*"\(.*\)"/\1/')
    E2E_USER_PASS=$(grep -o 'USER_PASS: &str = "[^"]*"' crates/pult-testkit/src/lib.rs | sed 's/.*"\(.*\)"/\1/')
    echo "fake DC at $DC"
fi
export E2E_SVC_PASS E2E_USER_PASS

echo "=== clean install $(dpkg-deb -f "$DEB" Version)"
( cd /tmp && runuser -u postgres -- dropdb --if-exists --force pult )
T0=$(date +%s.%N)
dpkg -i "$DEB" >/tmp/deb-e2e.log 2>&1 || { cat /tmp/deb-e2e.log; echo "=== FAIL install"; exit 1; }
for i in $(seq 1 30); do curl -fsk https://127.0.0.1/health >/dev/null 2>&1 && break; sleep 1; done
CODE=""
for i in $(seq 1 30); do
    CODE=$(journalctl -u pult --since "@${T0%.*}" --no-pager -o cat | grep "SETUP CODE" | tail -n 1 | sed 's/.*SETUP CODE: \([A-Za-z0-9_-]*\).*/\1/')
    [ -n "$CODE" ] && break; sleep 1
done
[ -n "$CODE" ] || { echo "=== FAIL no setup code"; exit 1; }
python3 -I "$HERE/deb-wizard-e2e.py" "$CODE" "$DC" /tmp/pult-e2e-ca.pem "$T0"
rc=$?
for s in "$E2E_SVC_PASS" "$E2E_USER_PASS"; do
    if journalctl -u pult --no-pager -o cat | grep -qF -- "$s"; then echo "=== FAIL secret in journal"; rc=1; fi
done
[ $rc -eq 0 ] && echo "ok   no test secrets in the service journal"
exit $rc
