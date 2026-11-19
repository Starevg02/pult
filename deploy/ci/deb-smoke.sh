#!/bin/sh
# Проверка пакета .deb на одноразовой машине Debian с systemd и PostgreSQL:
#   1) новая установка: служба на 443, код мастера в журнале, права на pult.env;
#   2) обновление: резервная копия перед миграциями, служба на новой версии;
#   3) сломанное обновление: пакет с ошибкой, база как была, служба работает на прежней версии.
#
#   deploy/ci/deb-smoke.sh pult_A.deb pult_B.deb pult_B-broken.deb
#
# ВНИМАНИЕ: ставит Pult в систему (роль и база pult в локальном PostgreSQL). Только для стенда CI.
set -eu

A=${1:?пакет A} B=${2:?пакет B} BROKEN=${3:?пакет со сломанной миграцией}
fail() { echo "=== FAIL: $*"; journalctl -u pult --no-pager -n 30 || true; exit 1; }
health() {
    i=0
    while [ $i -lt 30 ]; do
        curl -fsk https://127.0.0.1/health >/dev/null 2>&1 && return 0
        i=$((i + 1)); sleep 1
    done
    return 1
}
q() { runuser -u postgres -- psql -XtAq -d pult -c "$1"; }
ver() { /opt/pult/bin/pult version; }
pkgver() { dpkg-deb -f "$1" Version; }

echo "=== 1. установка $(pkgver "$A")"
dpkg -i "$A" >/tmp/deb-a.log 2>&1 || { cat /tmp/deb-a.log; fail "установка"; }
health || fail "служба не ответила после установки"
journalctl -u pult --no-pager | grep -q "SETUP CODE" || fail "нет кода мастера в журнале"
[ "$(stat -c '%U:%G %a' /etc/pult/pult.env)" = "root:pult 640" ] || fail "права pult.env: $(stat -c '%U:%G %a' /etc/pult/pult.env)"
[ -f /var/lib/pult/secret.key ] || fail "ключ шифрования не создан"
before=$(q "SELECT max(version) FROM _sqlx_migrations")
echo "ok: $(ver), миграции до $before"

echo "=== 2. обновление до $(pkgver "$B")"
copies=$(ls -d /var/lib/pult/backups/pult-* 2>/dev/null | wc -l)
dpkg -i "$B" >/tmp/deb-b.log 2>&1 || { cat /tmp/deb-b.log; fail "обновление"; }
health || fail "служба не ответила после обновления"
[ "$(ls -d /var/lib/pult/backups/pult-* | wc -l)" -gt "$copies" ] || fail "нет резервной копии перед обновлением"
[ -e /var/lib/pult-rollback ] && fail "каталог отката не убран"
good=$(ver)
echo "ok: $good, копия сделана"

echo "=== 3. сломанное обновление $(pkgver "$BROKEN")"
applied=$(q "SELECT count(*) FROM _sqlx_migrations")
if dpkg -i "$BROKEN" >/tmp/deb-c.log 2>&1; then cat /tmp/deb-c.log; fail "сломанное обновление установилось"; fi
grep -q "возвращена прежняя версия" /tmp/deb-c.log || { cat /tmp/deb-c.log; fail "нет отката бинарника"; }
[ "$(ver)" = "$good" ] || fail "бинарник не прежний: $(ver)"
[ "$(q "SELECT count(*) FROM _sqlx_migrations")" = "$applied" ] || fail "миграции изменились"
[ "$(q "SELECT to_regclass('public.smoke_broken') IS NULL")" = "t" ] || fail "таблица из сломанной миграции осталась"
health || fail "служба не работает после отката"
# Повтор настройки возвращает новую программу и пробует снова: сломанная миграция снова откатывается.
if dpkg --configure -a >/tmp/deb-c2.log 2>&1; then cat /tmp/deb-c2.log; fail "повтор сломанного обновления прошёл"; fi
grep -q "повтор обновления после отката" /tmp/deb-c2.log || { cat /tmp/deb-c2.log; fail "повтор не вернул новую программу"; }
[ "$(ver)" = "$good" ] || fail "после повтора бинарник не прежний: $(ver)"
[ "$(q "SELECT to_regclass('public.smoke_broken') IS NULL")" = "t" ] || fail "таблица из сломанной миграции осталась после повтора"
health || fail "служба не работает после повтора"
echo "ok: откат на $good, база не изменилась; повтор настройки тоже откатывается"

echo "=== 4. удаление (данные остаются) и повторная установка $(pkgver "$B")"
dpkg -r pult >/tmp/deb-d.log 2>&1 || { cat /tmp/deb-d.log; fail "удаление"; }
[ -f /var/lib/pult/secret.key ] || fail "при удалении пропал ключ шифрования"
copies=$(ls -d /var/lib/pult/backups/pult-* 2>/dev/null | wc -l)
dpkg -i "$B" >/tmp/deb-e.log 2>&1 || { cat /tmp/deb-e.log; fail "повторная установка"; }
grep -q "найдены данные прежней установки" /tmp/deb-e.log || { cat /tmp/deb-e.log; fail "нет копии при повторной установке"; }
[ "$(ls -d /var/lib/pult/backups/pult-* | wc -l)" -gt "$copies" ] || fail "копия не создана"
health || fail "служба не ответила после повторной установки"
[ -e /var/lib/pult-rollback ] && fail "каталог отката не убран"
echo "ok: копия перед миграциями, служба работает"
echo "=== DEB OK"
