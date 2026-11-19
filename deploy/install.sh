#!/bin/sh
# Pult: проверка сервера и установка (или обновление) пакета .deb.
#
#   sudo sh install.sh                          # пакет pult_*_amd64.deb рядом со скриптом или в текущем каталоге
#   sudo sh install.sh ./pult_0.1.0_amd64.deb   # конкретный файл
#   sudo sh install.sh --check                  # только проверки, ничего не ставить
#   sudo sh install.sh --dc dc1.corp.example.test --dc dc2.corp.example.test   # ещё проверить LDAPS контроллеров
#
# Скрипт необязателен: всё, что нужно, делает сам пакет (sudo apt install ./pult_…deb). Скрипт заранее
# находит то, из-за чего установка или первый вход не пройдут, и объясняет, что исправить.
# Интернет скрипту не нужен; PostgreSQL пакет ставит из репозитория дистрибутива (нужен доступ к зеркалу
# или локальный репозиторий), если его ещё нет.
set -eu

case "${LANG:-}${LC_ALL:-}" in *ru*) RU=1 ;; *) RU=0 ;; esac
t() { if [ "$RU" = 1 ]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }

FAILS=0
WARNS=0
ok() { printf '  [ OK ] %s\n' "$1"; }
warn() { WARNS=$((WARNS + 1)); printf '  [WARN] %s\n' "$1"; [ -z "${2:-}" ] || printf '         %s\n' "$2"; }
bad() { FAILS=$((FAILS + 1)); printf '  [FAIL] %s\n' "$1"; [ -z "${2:-}" ] || printf '         %s\n' "$2"; }
die() { printf 'pult: %s\n' "$1" >&2; exit 1; }

usage() {
    t "Использование: sudo sh install.sh [--check] [--dc ИМЯ_КОНТРОЛЛЕРА]... [пакет.deb]" \
      "Usage: sudo sh install.sh [--check] [--dc DOMAIN_CONTROLLER]... [package.deb]"
    echo
    exit "${1:-0}"
}

CHECK_ONLY=0
DCS=""
DEB=""
while [ $# -gt 0 ]; do
    case "$1" in
    --check) CHECK_ONLY=1 ;;
    --dc)
        [ $# -ge 2 ] || usage 2
        DCS="$DCS $2"
        shift
        ;;
    -h | --help) usage 0 ;;
    -*) usage 2 ;;
    *) DEB=$1 ;;
    esac
    shift
done

[ "$(id -u)" = 0 ] || die "$(t 'запустите от root: sudo sh install.sh' 'run as root: sudo sh install.sh')"

# --- Пакет: явно указанный или самый новый pult_*_amd64.deb рядом со скриптом / в текущем каталоге.
if [ -z "$DEB" ] && [ "$CHECK_ONLY" = 0 ]; then
    here=$(cd "$(dirname "$0")" && pwd)
    DEB=$(ls -1 "$here"/pult_*_amd64.deb ./pult_*_amd64.deb 2>/dev/null | sort -uV | tail -n 1 || true)
    [ -n "$DEB" ] || die "$(t 'не найден файл pult_*_amd64.deb: скачайте его со страницы релизов и укажите путь' \
        'pult_*_amd64.deb not found: download it from the releases page and pass its path')"
fi
if [ -n "$DEB" ]; then
    [ -f "$DEB" ] || die "$(t 'нет файла' 'no such file'): $DEB"
    case "$DEB" in /*) ;; *) DEB="$(pwd)/$DEB" ;; esac
fi

echo "$(t 'Проверка сервера' 'Checking the server')"

# --- Система
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    major=${VERSION_ID%%.*}
    case "${ID:-}" in
    debian) if [ "${major:-0}" -ge 13 ] 2>/dev/null; then ok "Debian ${VERSION_ID}"; else bad "Debian ${VERSION_ID:-?}" "$(t 'нужен Debian 13 или новее' 'Debian 13 or newer is required')"; fi ;;
    ubuntu) if [ "${major:-0}" -ge 24 ] 2>/dev/null; then ok "Ubuntu ${VERSION_ID}"; else bad "Ubuntu ${VERSION_ID:-?}" "$(t 'нужна Ubuntu 24.04 или новее' 'Ubuntu 24.04 or newer is required')"; fi ;;
    *) warn "${PRETTY_NAME:-$(t 'неизвестная система' 'unknown OS')}" "$(t 'проверено на Debian 13 и Ubuntu 24.04+; на других системах пакет может не встать' 'tested on Debian 13 and Ubuntu 24.04+; other systems may not work')" ;;
    esac
else
    warn "$(t 'не удалось определить систему (/etc/os-release)' 'cannot detect the OS (/etc/os-release)')"
fi

arch=$(dpkg --print-architecture 2>/dev/null || uname -m)
case "$arch" in
amd64 | x86_64) ok "$(t 'архитектура' 'architecture') $arch" ;;
*) bad "$(t 'архитектура' 'architecture') $arch" "$(t 'пакет собран только для amd64 (x86-64)' 'the package is built for amd64 (x86-64) only')" ;;
esac

if [ -d /run/systemd/system ]; then
    ok "systemd"
else
    bad "systemd" "$(t 'служба pult запускается через systemd; в контейнере используйте Docker-вариант (deploy/compose.yml)' 'pult runs as a systemd service; in a container use the Docker variant (deploy/compose.yml)')"
fi

mem_mb=$(awk '/^MemTotal:/ {print int($2 / 1024)}' /proc/meminfo 2>/dev/null || echo 0)
if [ "$mem_mb" -ge 1900 ]; then
    ok "$(t 'память' 'memory') ${mem_mb} MB"
else
    warn "$(t 'память' 'memory') ${mem_mb} MB" "$(t 'рекомендуется не меньше 2 ГБ (портал и PostgreSQL на одном сервере)' 'at least 2 GB is recommended (portal and PostgreSQL on one server)')"
fi

free_mb=$(df -Pm /var/lib 2>/dev/null | awk 'NR == 2 {print $4}')
if [ "${free_mb:-0}" -ge 5000 ]; then
    ok "$(t 'свободно в /var/lib' 'free in /var/lib') ${free_mb} MB"
else
    warn "$(t 'свободно в /var/lib' 'free in /var/lib') ${free_mb:-?} MB" "$(t 'рекомендуется не меньше 5 ГБ: база, вложения и 14 ночных копий' 'at least 5 GB is recommended: database, attachments and 14 nightly backups')"
fi

# --- Порт 443: занят не pult — служба не запустится.
if command -v ss >/dev/null 2>&1; then
    owner=$(ss -Hltnp 'sport = :443' 2>/dev/null | sed -n 's/.*users:(("\([^"]*\)".*/\1/p' | head -n 1)
    if [ -z "$owner" ] || [ "$owner" = pult ]; then
        ok "$(t 'порт 443 свободен для портала' 'port 443 is available for the portal')"
    else
        bad "$(t 'порт 443 занят программой' 'port 443 is used by') $owner" "$(t 'остановите её или задайте другой адрес в /etc/pult/pult.env (PULT_LISTEN) после установки' 'stop it, or set another address in /etc/pult/pult.env (PULT_LISTEN) after installation')"
    fi
fi

# --- Имя сервера: полное DNS-имя нужно для сертификата и входа без пароля (Kerberos).
fqdn=$(hostname -f 2>/dev/null || hostname)
case "$fqdn" in
*.*) ok "$(t 'имя сервера' 'server name') $fqdn" ;;
*) warn "$(t 'имя сервера' 'server name') $fqdn" "$(t 'нет полного DNS-имени (например pult.corp.example.test): нужно для сертификата и входа по Kerberos' 'not a fully qualified name (e.g. pult.corp.example.test): needed for the certificate and Kerberos sign-in')" ;;
esac

# --- Время: Kerberos и коды из приложения не работают при расхождении часов.
if command -v timedatectl >/dev/null 2>&1; then
    if [ "$(timedatectl show -p NTPSynchronized --value 2>/dev/null)" = yes ]; then
        ok "$(t 'время синхронизировано' 'clock is synchronized')"
    else
        warn "$(t 'время не синхронизировано' 'clock is not synchronized')" "$(t 'настройте NTP (лучше — на контроллеры домена): при расхождении больше 5 минут не работают Kerberos и коды из приложения' 'configure NTP (preferably against the domain controllers): Kerberos and authenticator codes fail with more than 5 minutes of skew')"
    fi
fi

# --- PostgreSQL: уже есть или пакет сможет поставить его из репозитория.
if command -v psql >/dev/null 2>&1 && runuser -u postgres -- psql -XtAqc 'SELECT 1' >/dev/null 2>&1; then
    ok "PostgreSQL $(runuser -u postgres -- psql -XtAqc 'SHOW server_version' 2>/dev/null | cut -d' ' -f1)"
elif apt-cache policy postgresql 2>/dev/null | grep -q 'Candidate: [0-9]'; then
    ok "$(t 'PostgreSQL будет установлен из репозитория' 'PostgreSQL will be installed from the repository')"
else
    warn "$(t 'PostgreSQL не найден и недоступен в репозиториях apt' 'PostgreSQL is not installed and not available from apt')" "$(t 'выполните apt update или подключите зеркало; для базы на другом сервере — docs/install.md, «База на другом сервере»' 'run apt update or add a mirror; for a database on another server see docs/install.md')"
fi

# --- Контроллеры домена: LDAPS (636) отвечает и отдаёт сертификат.
for dc in $DCS; do
    if ! command -v openssl >/dev/null 2>&1; then
        warn "$(t 'нет openssl — проверка контроллеров пропущена' 'openssl not found, domain controller check skipped')"
        break
    fi
    if timeout 8 openssl s_client -connect "$dc:636" -servername "$dc" </dev/null >/dev/null 2>&1; then
        ok "LDAPS $dc:636"
    elif timeout 8 sh -c "openssl s_client -connect '$dc:389' -starttls ldap </dev/null" >/dev/null 2>&1; then
        warn "LDAPS $dc:636" "$(t 'не отвечает, но работает StartTLS на 389 — в мастере выберите StartTLS' 'no answer, but StartTLS on 389 works: choose StartTLS in the wizard')"
    else
        bad "LDAPS $dc:636" "$(t 'нет защищённого подключения: проверьте имя, DNS, сетевой доступ и сертификат контроллера (docs/install.md, «Контроллеры не отвечают по LDAPS»)' 'no secure connection: check the name, DNS, network access and the controller certificate (docs/install.md)')"
    fi
done

# --- Пакет: контрольная сумма, если рядом лежит .sha256 или SHA256SUMS.
if [ -n "$DEB" ]; then
    dir=$(dirname "$DEB")
    name=$(basename "$DEB")
    sums=""
    if [ -f "$DEB.sha256" ]; then
        sums="$DEB.sha256"
    elif [ -f "$dir/SHA256SUMS" ] && grep -q " $name\$" "$dir/SHA256SUMS"; then
        sums="$dir/SHA256SUMS"
    fi
    if [ -n "$sums" ]; then
        if (cd "$dir" && grep " $name\$" "$sums" | sha256sum -c --status); then
            ok "$(t 'контрольная сумма пакета' 'package checksum') $name"
        else
            bad "$(t 'контрольная сумма пакета не совпала' 'package checksum mismatch') $name" "$(t 'файл повреждён или подменён — скачайте заново' 'the file is damaged or replaced: download it again')"
        fi
    else
        warn "$(t 'нет файла контрольной суммы для' 'no checksum file for') $name" "$(t 'положите рядом SHA256SUMS со страницы релиза' 'put SHA256SUMS from the release page next to it')"
    fi
fi

echo
if [ "$FAILS" -gt 0 ]; then
    die "$(t "проверки не пройдены: $FAILS — исправьте отмеченное [FAIL] и запустите снова" "$FAILS check(s) failed: fix the [FAIL] items and run again")"
fi
[ "$WARNS" -eq 0 ] || echo "$(t "предупреждений: $WARNS — установка возможна, но посмотрите [WARN]" "$WARNS warning(s): installation can proceed, but review the [WARN] items")"
[ "$CHECK_ONLY" = 0 ] || exit 0

# --- Установка или обновление
prev=$(dpkg-query -W -f='${Version}' pult 2>/dev/null || true)
if [ -n "$prev" ]; then
    echo "$(t 'Обновление Pult с версии' 'Upgrading Pult from') $prev"
else
    echo "$(t 'Установка Pult' 'Installing Pult')"
fi
DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l apt-get install -y "$DEB"

echo
if [ "$(systemctl is-active pult.service 2>/dev/null)" != active ]; then
    die "$(t 'служба pult не запущена — смотрите вывод выше и journalctl -u pult' 'the pult service is not running: see the output above and journalctl -u pult')"
fi
echo "$(t 'Готово:' 'Done:') $(/opt/pult/bin/pult version)"
if [ -z "$prev" ]; then
    port=$(sed -n 's/^PULT_LISTEN=.*:\([0-9]*\)$/\1/p' /etc/pult/pult.env 2>/dev/null | tail -n 1)
    url="https://$fqdn/"
    [ -z "$port" ] || [ "$port" = 443 ] || url="https://$fqdn:$port/"
    echo "$(t 'Откройте в браузере' 'Open in a browser'): $url"
    echo "$(t 'Мастер первого запуска спросит код установки:' 'The first-run wizard asks for the setup code:')"
    sleep 2
    code=$(journalctl -u pult --no-pager -o cat 2>/dev/null | grep 'SETUP CODE' | tail -n 1 |
        sed -n 's/.*SETUP CODE: \([A-Za-z0-9_-]*\).*/\1/p' || true)
    if [ -n "$code" ]; then
        echo "  $code"
    else
        echo "  journalctl -u pult | grep 'SETUP CODE' | tail -n 1"
    fi
    echo "$(t 'Дальше — docs/install.md, раздел «Мастер первого запуска».' 'Next: docs/install.en.md, section "First-run wizard".')"
fi
