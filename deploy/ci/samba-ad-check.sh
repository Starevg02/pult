#!/bin/sh
# Вход и синхронизация на Samba AD из compose.dev.yml: вход, коды отказа, StartTLS,
# переключение на второй DC, полная и инкрементальная синхронизация.
#
#   docker compose -f deploy/compose.dev.yml up -d --build
#   deploy/ci/samba-ad-check.sh target/debug/pult
#
# Нужны записи в hosts: 127.0.0.1 dc1.pult.test dc2.pult.test (имя должно совпадать с сертификатом).
# Пароли — тестового домена из compose.dev.yml и seed-users.sh, переопределяются переменными.
# Скрипт останавливает samba-dc1 (проверка переключения) и в конце запускает его снова.
set -u

bin="${1:?путь к бинарнику pult}"
compose="docker compose -f deploy/compose.dev.yml"
svc_pass="${PULT_DEV_SVC_PASSWORD:-Pult-Svc-2026!}"
user_pass="${PULT_DEV_USER_PASSWORD:-Pult-Test-2026!}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failed=0

echo "== Жду контроллеры и тестовые учётки"
i=0
until [ -f deploy/dev/ca/dc1-ca.pem ] && [ -f deploy/dev/ca/dc2-ca.pem ] &&
	$compose exec -T samba-dc1 test -f /var/lib/samba/.seeded 2>/dev/null; do
	i=$((i + 1))
	if [ "$i" -gt 90 ]; then
		echo "контроллеры не поднялись за 6 минут"
		exit 1
	fi
	sleep 4
done
cat deploy/dev/ca/dc1-ca.pem deploy/dev/ca/dc2-ca.pem >deploy/dev/ca/all-ca.pem

# Конфиги: оба DC по LDAPS, только второй DC, оба по StartTLS.
cp deploy/pult.example.toml "$work/ldaps.toml"
sed 's|^controllers = .*|controllers = ["dc2.pult.test:6362"]|' "$work/ldaps.toml" >"$work/dc2.toml"
sed 's|^controllers = .*|controllers = ["dc1.pult.test:3891", "dc2.pult.test:3892"]|; s|^tls = "ldaps"|tls = "starttls"|' \
	"$work/ldaps.toml" >"$work/starttls.toml"
export PULT_AD_BIND_PASSWORD="$svc_pass"

# check CONFIG ОЖИДАНИЕ [аргументы check ad...] — запускает `pult check ad` и ищет ОЖИДАНИЕ в выводе.
check() {
	cfg="$1" want="$2" title="$3"
	shift 3
	out="$("$bin" --config "$work/$cfg" check ad "$@" 2>&1)"
	if printf '%s' "$out" | grep -q -- "$want"; then
		echo "✅ $title"
	else
		echo "❌ $title — ждали «$want», получили:"
		printf '%s\n' "$out" | tail -n 5 | sed 's/^/     /'
		failed=$((failed + 1))
	fi
}
login() {
	cfg="$1" user="$2" pass="$3" want="$4" title="$5"
	PULT_CHECK_AD_PASSWORD="$pass" check "$cfg" "$want" "$title" --login "$user"
}

echo "== Реплика получила учётки"
i=0
until PULT_CHECK_AD_PASSWORD="$user_pass" "$bin" --config "$work/dc2.toml" check ad --login ivanov 2>&1 | grep -q "Вход выполнен"; do
	i=$((i + 1))
	if [ "$i" -gt 45 ]; then
		echo "❌ учётки не дошли до dc2 за 3 минуты"
		failed=$((failed + 1))
		break
	fi
	sleep 4
done

echo "== Вход и причины отказа"
login ldaps.toml ivanov "$user_pass" "Вход выполнен.*dc1.pult.test:6361" "вход по LDAPS через первый DC"
login ldaps.toml 'PULT\petrova' "$user_pass" "Вход выполнен" "вход в форме ДОМЕН\\логин"
login ldaps.toml ivanov@pult.test "$user_pass" "Вход выполнен" "вход по UPN"
login ldaps.toml ivanov "wrong-password" "\[InvalidCredentials\]" "неверный пароль"
login ldaps.toml nobody "$user_pass" "\[InvalidCredentials\]" "нет такого логина — тот же ответ"
login ldaps.toml disabled "$user_pass" "\[AccountDisabled\]" "отключённая учётка (533)"
login ldaps.toml mustchange "$user_pass" "\[MustChangePassword\]" "нужно сменить пароль (773)"
login ldaps.toml expired "$user_pass" "\[AccountExpired\]" "истёк срок учётки (701)"
login starttls.toml ivanov "$user_pass" "Вход выполнен.*dc1.pult.test:3891" "вход по StartTLS"

echo "== Синхронизация"
full="$("$bin" --config "$work/ldaps.toml" check ad --page-size 2 2>&1)"
usn="$(printf '%s' "$full" | sed -n 's/.*highestCommittedUSN \([0-9][0-9]*\).*/\1/p' | tail -n 1)"
if printf '%s' "$full" | grep -q "Полная синхронизация через dc1.pult.test:6361: учёток" && [ -n "$usn" ]; then
	echo "✅ полная синхронизация страницами по 2 ($(printf '%s' "$full" | grep -o 'учёток [0-9]*'), USN $usn)"
else
	echo "❌ полная синхронизация:"
	printf '%s\n' "$full" | tail -n 5 | sed 's/^/     /'
	failed=$((failed + 1))
	usn=0
fi
check ldaps.toml "Инкрементальная синхронизация.*учёток 0" "инкрементальная сразу после полной — изменений нет" --since-usn "$usn"
$compose exec -T samba-dc1 samba-tool user setexpiry petrova --noexpiry >/dev/null
check ldaps.toml "Инкрементальная синхронизация.*учёток [1-9]" "изменение учётки попадает в инкрементальную" --since-usn "$usn"

echo "== Остановка первого DC"
$compose stop samba-dc1 >/dev/null
login ldaps.toml ivanov "$user_pass" "Вход выполнен.*dc2.pult.test:6362" "первый DC остановлен — вход через второй"
check ldaps.toml "Полная синхронизация через dc2.pult.test:6362" "синхронизация через второй DC"
$compose start samba-dc1 >/dev/null

if [ "$failed" -gt 0 ]; then
	echo "Не прошло проверок: $failed"
	exit 1
fi
echo "Все проверки на Samba AD пройдены"
