#!/bin/sh
# Мастер первого запуска на Samba AD: чистая база, без ключа и сертификата в настройках.
#
#   deploy/ci/setup-wizard-samba.sh target/debug/pult
#
# Нужны: PostgreSQL и Samba из compose.dev.yml, записи hosts dc1/dc2.pult.test, all-ca.pem, jq, openssl.
set -u

bin="${1:?путь к бинарнику pult}"
compose="docker compose -f deploy/compose.dev.yml"
svc_pass="${PULT_DEV_SVC_PASSWORD:-Pult-Svc-2026!}"
user_pass="${PULT_DEV_USER_PASSWORD:-Pult-Test-2026!}"
base="https://127.0.0.1:18443"
work="$(mktemp -d)"
log="$work/serve.log"
jar="$work/admin.jar"
failed=0
pid=""
cleanup() {
	[ -n "$pid" ] && kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null
	$compose exec -T postgres dropdb -U pult --if-exists pult_wizard >/dev/null 2>&1
	rm -rf "$work"
}
trap cleanup EXIT

ok() { echo "✅ $1"; }
bad() {
	echo "❌ $1"
	[ -n "${2:-}" ] && printf '%s\n' "$2" | tail -n 5 | sed 's/^/     /'
	failed=$((failed + 1))
}

$compose exec -T postgres createdb -U pult pult_wizard || exit 1
unset PULT_SECRET_KEY PULT_INITIAL_SUPERADMIN PULT_INSECURE_COOKIES
export PULT_DATABASE_URL="postgres://pult:pult-dev@localhost:5432/pult_wizard"
export PULT_DATA_DIR="$work/data" PULT_AUTO_TLS=1 PULT_LISTEN=127.0.0.1:18443 PULT_LOG_FORMAT=text NO_COLOR=1
# GreenMail в окружении разработчика работает без TLS.
export PULT_INSECURE_MAIL=1

"$bin" migrate >/dev/null 2>&1 || { bad "миграции"; exit 1; }
start() {
	"$bin" serve >>"$log" 2>&1 &
	pid=$!
	i=0
	until curl -kfsS "$base/health" >/dev/null 2>&1; do
		i=$((i + 1))
		[ "$i" -gt 60 ] && { bad "сервер не поднялся" "$(tail -n 20 "$log")"; exit 1; }
		sleep 1
	done
}
start
ok "сервер поднялся без ключа и сертификата в настройках"

mode() { stat -c %a "$1" 2>/dev/null; }
[ "$(mode "$work/data/secret.key")" = 600 ] && ok "ключ шифрования создан (600)" || bad "secret.key"
[ "$(mode "$work/data/tls/key.pem")" = 600 ] && ok "ключ сертификата создан (600)" || bad "tls/key.pem"

# Код печатается, когда база уже доступна, — подождём.
i=0
until code="$(grep 'SETUP CODE' "$log" | tail -n 1 | sed 's/.*SETUP CODE: \([^ ]*\).*/\1/')"; [ -n "$code" ]; do
	i=$((i + 1))
	[ "$i" -gt 30 ] && { bad "кода настройки нет в журнале" "$(tail -n 20 "$log")"; exit 1; }
	sleep 1
done
ok "код настройки в журнале"

# req МЕТОД ПУТЬ [JSON] — запрос с кодом мастера; печатает «ТЕЛО КОД».
req() {
	curl -ksS -X "$1" -H "Content-Type: application/json" -H "X-Setup-Token: ${token-$code}" \
		-b "$jar" -c "$jar" ${3:+--data "$3"} -w ' %{http_code}' "$base$2"
}
status() { printf '%s' "$1" | awk '{print $NF}'; }
# api МЕТОД ПУТЬ [JSON] — запрос администратора с CSRF-токеном из cookie.
api() {
	csrf="$(awk '$6 == "__Host-pult_csrf" {print $7}' "$jar" 2>/dev/null)"
	curl -ksS -b "$jar" -c "$jar" -X "$1" -H "Content-Type: application/json" -H "X-CSRF-Token: $csrf" \
		${3:+--data "$3"} -w ' %{http_code}' "$base$2"
}

domain() { # domain ПАРОЛЬ_СЛУЖЕБНОЙ
	jq -n --arg p "$1" --rawfile ca deploy/dev/ca/all-ca.pem \
		'{dns_name:"pult.test",controllers:["dc1.pult.test:6361","dc2.pult.test:6362"],tls_mode:"ldaps",bind_dn:"svc-pult@pult.test",bind_password:$p,ca_cert_pem:$ca}'
}
finish() { # finish ЛОГИН ПАРОЛЬ
	domain "$svc_pass" | jq -c --arg l "$1" --arg p "$2" '{domain:.,admin_login:$l,admin_password:$p}'
}

echo "== Код"
r="$(curl -ksS "$base/api/setup/status")"
printf '%s' "$r" | grep -q '"needed":true,"has_domain":false' && ok "мастер открыт" || bad "статус" "$r"
[ "$(status "$(token=wrong req POST /api/setup/verify)")" = 403 ] && ok "неверный код — 403" || bad "неверный код"
[ "$(status "$(token=wrong req POST /api/setup/test "$(domain x)")")" = 403 ] && ok "без кода домен не проверить" || bad "test без кода"
[ "$(status "$(req POST /api/setup/verify)")" = 204 ] && ok "верный код" || bad "верный код"

echo "== Домен"
r="$(req POST /api/setup/test "$(domain wrong-password)")"
printf '%s' "$r" | grep -q 'Неверный логин или пароль служебной' && ok "неверный пароль службы — понятная подсказка" || bad "подсказка" "$r"
r="$(req POST /api/setup/test "$(domain "$svc_pass")")"
printf '%s' "$r" | grep -q '"ok":true' && ok "контроллеры отвечают" || bad "проверка домена" "$r"

echo "== Завершение"
r="$(req POST /api/setup/complete "$(finish ivanov wrong-password)")"
printf '%s' "$r" | grep -q 'admin_rejected' && ok "неверный пароль администратора отклонён" || bad "админ с неверным паролем" "$r"
r="$(curl -ksS "$base/api/setup/status")"
printf '%s' "$r" | grep -q '"has_domain":false' && ok "после отказа ничего не сохранено" || bad "домен сохранён при отказе" "$r"
r="$(req POST /api/setup/complete "$(finish "PULT\\ivanov" "$user_pass")")"
[ "$(status "$r")" = 204 ] && ok "настройка завершена" || bad "завершение" "$r"
r="$(api GET /api/me)"
printf '%s' "$r" | grep -q '"is_superadmin":true' && ok "администратор вошёл и стал суперадминистратором" || bad "суперадминистратор" "$r"
r="$(curl -ksS "$base/api/setup/status")"
printf '%s' "$r" | grep -q '"needed":false' && ok "мастер закрыт" || bad "мастер не закрылся" "$r"
[ "$(status "$(req POST /api/setup/verify)")" = 404 ] && ok "код больше не работает" || bad "код после завершения"
[ "$(status "$(req POST /api/setup/complete "$(finish petrova "$user_pass")")")" = 404 ] && ok "второго завершения нет" || bad "второе завершение"

echo "== Первая синхронизация"
i=0
until r="$(api GET '/api/phonebook/persons?q=bdfyjd')"; printf '%s' "$r" | grep -q 'Иванов'; do
	i=$((i + 1))
	[ "$i" -gt 40 ] && break
	sleep 2
done
printf '%s' "$r" | grep -q 'Иванов' && ok "сотрудники появились без ручного запуска" || bad "синхронизация" "$r"

echo "== Сертификат"
served() { echo | openssl s_client -connect 127.0.0.1:18443 2>/dev/null | openssl x509 -noout -subject; }
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 -subj "/CN=wizard.pult.test" \
	-addext "subjectAltName=DNS:wizard.pult.test" -keyout "$work/k.pem" -out "$work/c.pem" 2>/dev/null
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 -subj "/CN=other" -keyout "$work/k2.pem" -out "$work/c2.pem" 2>/dev/null
body() { jq -n --rawfile c "$1" --rawfile k "$2" '{cert_pem:$c,key_pem:$k}'; }
r="$(api PUT /api/admin/tls "$(body "$work/c.pem" "$work/k2.pem")")"
[ "$(status "$r")" = 400 ] && ok "чужой ключ отклонён" || bad "чужой ключ" "$r"
r="$(api PUT /api/admin/tls "$(body "$work/c.pem" "$work/k.pem")")"
[ "$(status "$r")" = 200 ] && ok "сертификат заменён" || bad "замена сертификата" "$r"
served | grep -q wizard.pult.test && ok "новый сертификат работает без перезапуска" || bad "сертификат не применился" "$(served)"

echo "== Почта: уведомление доходит по SMTP (GreenMail)"
r="$(api PUT /api/admin/mail '{"settings":{"enabled":true,"host":"127.0.0.1","port":3025,"security":"none","username":"","from_address":"helpdesk@pult.test","from_name":"Pult"},"password":null}')"
[ "$(status "$r")" = 200 ] && ok "SMTP настроен" || bad "SMTP" "$r"
site="$(api POST /api/admin/sites '{"name":"Почтовая","timezone":"Europe/Moscow","is_active":true}' | sed 's/ [0-9]*$//' | jq -r .id)"
cat="$(api GET /api/helpdesk/categories | sed 's/ [0-9]*$//' | jq -r '.[0].id')"
r="$(api POST /api/helpdesk/tickets "$(jq -cn --arg s "$site" --arg c "$cat" '{title:"Проверка уведомления",description:"x",site_id:$s,category_id:$c}')")"
number="$(printf '%s' "$r" | sed 's/ [0-9]*$//' | jq -r '.number // empty')"
[ -n "$number" ] && ok "заявка #$number создана" || bad "заявка" "$r"
i=0
found=""
while [ "$i" -lt 45 ]; do
	found="$(curl -s "imap://127.0.0.1:3143/INBOX;UID=1" -u ivanov@pult.test:ivanov@pult.test || true)"
	printf '%s' "$found" | grep -q 'References:.*ticket-' && break
	i=$((i + 1))
	sleep 3
done
printf '%s' "$found" | grep -q 'References:.*ticket-' && ok "уведомление доставлено с заголовками цепочки" || bad "письмо заявителю" "$found"

echo "== Перезапуск"
kill "$pid"; wait "$pid" 2>/dev/null; pid=""
before="$(grep -c 'SETUP CODE' "$log")"
start
sleep 2
served | grep -q wizard.pult.test && ok "сертификат сохранился" || bad "сертификат после перезапуска"
[ "$(grep -c 'SETUP CODE' "$log")" = "$before" ] && ok "код настройки больше не печатается" || bad "код печатается после настройки"
rm -f "$jar"
r="$(curl -ksS -c "$jar" -H 'Content-Type: application/json' -w ' %{http_code}' --data "$(jq -cn --arg p "$user_pass" '{login:"ivanov",password:$p}')" "$base/api/auth/login")"
[ "$(status "$r")" = 204 ] && ok "вход после перезапуска: ключ шифрования тот же" || bad "вход после перезапуска" "$r"

for s in "$svc_pass" "$user_pass" "PRIVATE KEY-----"; do
	if grep -qF -- "$s" "$log"; then bad "секрет попал в журнал сервера"; else ok "секрета нет в журнале сервера"; fi
done

if [ "$failed" -gt 0 ]; then
	echo "Не прошло проверок: $failed"
	tail -n 30 "$log"
	exit 1
fi
echo "Мастер первого запуска на Samba AD работает"
