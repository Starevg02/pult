#!/bin/sh
# Смоук release-бинарника со встроенным фронтендом.
#
#   (cd web && npm ci && npm run build)
#   cargo build --release -p pult-server
#   deploy/ci/smoke-release.sh target/release/pult
#
# Перед запуском web/dist убирается с диска: так видно, что файлы вшиты в бинарник, а не читаются
# с диска. Отданное сервером сравнивается побайтно с собранным. БД не нужна: /health отвечает 503.
set -eu

bin="${1:?путь к бинарнику}"
[ -f web/dist/index.html ] || {
	echo "нет web/dist/index.html — сначала соберите фронтенд: (cd web && npm ci && npm run build)"
	exit 2
}
asset="$(cd web/dist && ls assets/*.js | head -n 1)"
[ -n "$asset" ] || {
	echo "в web/dist/assets нет .js"
	exit 2
}
sum() { sha256sum | cut -d' ' -f1; }
index_sum="$(sum <web/dist/index.html)"
asset_sum="$(sum <"web/dist/$asset")"

port="${SMOKE_PORT:-18090}"
base="http://127.0.0.1:$port"
work="$(mktemp -d)"
cp "$bin" "$work/"
mv web/dist "$work/dist.moved"
restore() {
	if [ -n "${pid:-}" ]; then
		kill "$pid" 2>/dev/null || true
		wait "$pid" 2>/dev/null || true
	fi
	if [ -d "$work/dist.moved" ]; then mv "$work/dist.moved" web/dist; fi
	rm -rf "$work"
}
trap restore EXIT INT TERM

# Секрет — только для запуска смоука, генерируется на месте.
key="$(head -c 48 /dev/urandom | od -An -tx1 | tr -d ' \n')"
# exec: в $pid — сам сервер, а не промежуточная оболочка (иначе kill оставил бы сервер сиротой).
(cd "$work" && PULT_SECRET_KEY="$key" PULT_LISTEN="127.0.0.1:$port" \
	PULT_DATABASE_URL="postgres://smoke@127.0.0.1:1/none" exec "./$(basename "$bin")" serve >serve.log 2>&1) &
pid=$!
i=0
until curl -s -o /dev/null "$base/health"; do
	i=$((i + 1))
	if [ "$i" -gt 50 ]; then
		echo "сервер не поднялся:"
		cat "$work/serve.log"
		exit 1
	fi
	sleep 0.2
done

fail() {
	echo "ОШИБКА: $*"
	exit 1
}
header() { curl -s -D - -o /dev/null "$base$1" | tr -d '\r' | grep -i "^$2:" | head -n 1; }
status() { curl -s -o /dev/null -w '%{http_code}' "$base$1"; }

[ "$(curl -fsS "$base/" | sum)" = "$index_sum" ] || fail "/ отдаёт не тот index.html, что собран"
header / cache-control | grep -q no-cache || fail "index.html кешируется"
[ "$(curl -fsS "$base/tickets/1300" | sum)" = "$index_sum" ] || fail "маршрут SPA не отдаёт index.html"
[ "$(curl -fsS "$base/$asset" | sum)" = "$asset_sum" ] || fail "встроенный $asset отличается от собранного"
header "/$asset" cache-control | grep -q immutable || fail "$asset без долгого кеша"
header "/$asset" content-type | grep -q javascript || fail "$asset с неверным типом"
[ "$(status /assets/missing-ffff.js)" = 404 ] || fail "отсутствующий файл не 404"
[ "$(status /api/nope)" = 404 ] || fail "/api/* перехвачен фронтендом"
[ "$(status /health)" = 503 ] || fail "/health без БД должен отвечать 503"
header / content-security-policy | grep -q "default-src 'self'" || fail "нет CSP"
echo "✅ release-бинарник: фронтенд встроен и раздаётся ($asset, $(du -h "$bin" | cut -f1))"
