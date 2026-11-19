#!/bin/sh
# Портал на Samba AD из compose.dev.yml: настоящий `pult serve`, домен через `pult setup ad`,
# вход по HTTP, синхронизация, поиск с исправлением раскладки, отключение учётки в AD →
# сеанс закрыт и запись в справочнике в архиве.
#
#   deploy/ci/stage1-samba.sh target/debug/pult
#
# Нужны: PULT_DATABASE_URL (PostgreSQL с миграциями), записи hosts dc1/dc2.pult.test, all-ca.pem
# (их готовит deploy/ci/samba-ad-check.sh). Пароли — тестового домена из seed-users.sh.
set -u

bin="${1:?путь к бинарнику pult}"
compose="docker compose -f deploy/compose.dev.yml"
svc_pass="${PULT_DEV_SVC_PASSWORD:-Pult-Svc-2026!}"
user_pass="${PULT_DEV_USER_PASSWORD:-Pult-Test-2026!}"
base="http://127.0.0.1:18080"
work="$(mktemp -d)"
failed=0
pid=""
cleanup() {
	[ -n "$pid" ] && kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null
	for dc in samba-dc1 samba-dc2; do $compose exec -T "$dc" samba-tool user enable petrova >/dev/null 2>&1; done
	rm -rf "$work"
}
trap cleanup EXIT

ok() { echo "✅ $1"; }
bad() {
	echo "❌ $1"
	[ -n "${2:-}" ] && printf '%s\n' "$2" | tail -n 5 | sed 's/^/     /'
	failed=$((failed + 1))
}

export PULT_SECRET_KEY="ci-stage1-$(od -An -tx1 -N24 /dev/urandom | tr -d ' \n')"
export PULT_LISTEN=127.0.0.1:18080 PULT_INSECURE_COOKIES=1 PULT_INITIAL_SUPERADMIN=ivanov PULT_LOG_FORMAT=text

echo "== pult setup ad"
out="$(PULT_AD_BIND_PASSWORD="$svc_pass" "$bin" setup ad --base-dn "DC=pult,DC=test" \
	--controllers dc1.pult.test:6361,dc2.pult.test:6362 --bind-dn svc-pult@pult.test \
	--ca-cert deploy/dev/ca/all-ca.pem --netbios PULT 2>&1)"
printf '%s' "$out" | grep -q "Домен сохранён" && ok "домен сохранён (пароль — зашифрованным)" || bad "pult setup ad" "$out"

echo "== pult serve"
# Синхронизацию запускаем сами — фоновые задачи (расписание) выключены, чтобы не столкнуться.
printf '[jobs]
enabled = false
' >"$work/pult.toml"
"$bin" --config "$work/pult.toml" serve >"$work/serve.log" 2>&1 &
pid=$!
i=0
until curl -fsS "$base/health" >/dev/null 2>&1; do
	i=$((i + 1))
	[ "$i" -gt 60 ] && { bad "сервер не поднялся" "$(tail -n 20 "$work/serve.log")"; exit 1; }
	sleep 1
done
ok "сервер отвечает"

# api JAR МЕТОД ПУТЬ [JSON] — запрос с cookie и CSRF-токеном из cookie; печатает «КОД ТЕЛО».
api() {
	jar="$work/$1.jar" method="$2" path="$3" body="${4:-}"
	csrf="$(awk '$6 == "pult_csrf" {print $7}' "$jar" 2>/dev/null)"
	curl -sS -b "$jar" -c "$jar" -X "$method" -H "Content-Type: application/json" -H "X-CSRF-Token: $csrf" \
		${body:+--data "$body"} -w ' %{http_code}' "$base$path"
}
code() { printf '%s' "$1" | awk '{print $NF}'; }

login() {
	r="$(api "$1" POST /api/auth/login "{\"login\":\"$2\",\"password\":\"$3\"}")"
	code "$r"
}

echo "== Вход паролем AD"
[ "$(login admin ivanov wrong-password)" = 401 ] && ok "неверный пароль — 401" || bad "неверный пароль"
[ "$(login admin ivanov "$user_pass")" = 204 ] && ok "ivanov вошёл" || bad "вход ivanov"
r="$(api admin GET /api/me)"
printf '%s' "$r" | grep -q '"is_superadmin":true' && ok "первый суперадминистратор (PULT_INITIAL_SUPERADMIN)" || bad "суперадминистратор" "$r"
r="$(curl -sS -b "$work/admin.jar" -X POST -H "Content-Type: application/json" --data '{"ids":[]}' -w ' %{http_code}' "$base/api/notifications/read")"
[ "$(code "$r")" = 403 ] && ok "без CSRF-токена — 403" || bad "CSRF" "$r"

echo "== Синхронизация и справочник"
r="$(api admin POST /api/admin/ad/sync '{"kind":"full"}')"
[ "$(code "$r")" = 200 ] && printf '%s' "$r" | grep -q '"seen":[1-9]' && ok "полная синхронизация: $(printf '%s' "$r" | grep -o '"seen":[0-9]*')" || bad "синхронизация" "$r"
r="$(api admin GET '/api/phonebook/persons?q=bdfyjd')"
printf '%s' "$r" | grep -q 'Иванов' && ok "поиск «bdfyjd» находит Иванова" || bad "поиск по раскладке" "$r"

echo "== Компьютеры домена"
r="$(api admin GET '/api/helpdesk/computers?q=dc1')"
printf '%s' "$r" | grep -qi '"name":"DC1"' && ok "контроллер домена найден среди компьютеров" || bad "компьютеры из Samba AD" "$r"

echo "== Отключение учётки в AD"
[ "$(login petrova petrova "$user_pass")" = 204 ] && ok "petrova вошла" || bad "вход petrova"
# На обоих DC: сервер читает с того, что ответил первым, а репликация между ними может отставать.
# Если отключение с первого DC успело реплицироваться, второй ответит «no matching attribute value» — это не сбой.
for dc in samba-dc1 samba-dc2; do
	out="$($compose exec -T "$dc" samba-tool user disable petrova 2>&1)" ||
		printf '%s' "$out" | grep -q 'no matching attribute value' ||
		bad "samba-tool user disable на $dc" "$out"
done
ok "petrova отключена в Samba AD (оба DC)"
# Изменение может ещё не дойти до того DC, с которого читает сервер, — несколько попыток.
i=0
until r="$(api admin POST /api/admin/ad/sync '{"kind":"incremental"}')"; printf '%s' "$r" | grep -q '"disabled":1'; do
	i=$((i + 1))
	[ "$i" -gt 20 ] && break
	sleep 3
done
printf '%s' "$r" | grep -q '"disabled":1' && ok "синхронизация увидела отключение" || bad "инкрементальная синхронизация" "$r"
[ "$(code "$(api petrova GET /api/me)")" = 401 ] && ok "сеанс petrova закрыт" || bad "сеанс petrova не закрыт"
r="$(api admin GET '/api/phonebook/persons?archived=true&q=petrova')"
printf '%s' "$r" | grep -q 'Петрова' && ok "запись Петровой в архиве" || bad "архив" "$r"

if grep -q "$user_pass" "$work/serve.log"; then bad "пароль попал в журнал сервера"; else ok "пароль не попал в журнал сервера"; fi

if [ "$failed" -gt 0 ]; then
	echo "Не прошло проверок: $failed"
	tail -n 30 "$work/serve.log"
	exit 1
fi
echo "Все проверки портала на Samba AD пройдены"
