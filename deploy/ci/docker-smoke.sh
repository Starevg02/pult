#!/bin/sh
# Установка из deploy/compose.yml, как у пользователя: без настроек поднимается HTTPS и открывается мастер.
#   docker compose -f deploy/compose.yml up -d --build && deploy/ci/docker-smoke.sh
set -u

compose="docker compose -f deploy/compose.yml"
base="https://127.0.0.1"
failed=0
ok() { echo "✅ $1"; }
bad() {
	echo "❌ $1"
	[ -n "${2:-}" ] && printf '%s\n' "$2" | tail -n 5 | sed 's/^/     /'
	failed=$((failed + 1))
}

i=0
until curl -kfsS "$base/health" >/dev/null 2>&1; do
	i=$((i + 1))
	[ "$i" -gt 60 ] && { bad "портал не поднялся" "$($compose logs --no-color --tail 30 pult)"; exit 1; }
	sleep 2
done
ok "HTTPS отвечает без какой-либо настройки"

r="$(curl -ksS "$base/api/setup/status")"
printf '%s' "$r" | grep -q '"needed":true' && ok "открыт мастер первого запуска" || bad "мастер" "$r"

code="$($compose logs --no-color pult | grep 'SETUP CODE' | tail -n 1 | sed 's/.*SETUP CODE: \([^ ]*\).*/\1/')"
[ -n "$code" ] && ok "код настройки есть в журнале" || bad "кода нет в журнале" "$($compose logs --no-color --tail 30 pult)"
c="$(curl -ks -o /dev/null -w '%{http_code}' -X POST -H "X-Setup-Token: wrong" "$base/api/setup/verify")"
[ "$c" = 403 ] && ok "неверный код — 403" || bad "неверный код: $c"
c="$(curl -ks -o /dev/null -w '%{http_code}' -X POST -H "X-Setup-Token: $code" "$base/api/setup/verify")"
[ "$c" = 204 ] && ok "верный код принят" || bad "верный код: $c"

curl -ksS "$base/" | grep -q '<div id="root">' && ok "фронтенд отдаётся" || bad "фронтенд"
$compose exec -T pult sh -c 'test "$(stat -c %a /data/secret.key)" = 600 && test "$(stat -c %a /data/tls/key.pem)" = 600' &&
	ok "ключи доступны только владельцу" || bad "права на ключи"
[ "$($compose exec -T pult id -u)" = 10001 ] && ok "сервер работает не под root" || bad "пользователь контейнера"

r="$($compose exec -T pult pult backup 2>&1)"
printf '%s' "$r" | grep -q 'Резервная копия' && ok "pult backup работает" || bad "pult backup" "$r"
$compose exec -T pult sh -c 'test -s /data/backups/pult-*/db.dump && ! test -e /data/backups/pult-*/secret.key' && ok "в копии есть база, ключа нет" || bad "содержимое копии"

if [ "$failed" -gt 0 ]; then
	echo "Не прошло проверок: $failed"
	exit 1
fi
echo "Установка из compose работает"
