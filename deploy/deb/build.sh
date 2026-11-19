#!/bin/sh
# Сборка пакета Pult для Debian/Ubuntu (amd64): один файл .deb — и установка, и обновление,
# в том числе без интернета. Веб-интерфейс и миграции встроены в бинарник.
#
#   deploy/deb/build.sh target/release/pult target/deb
#
# Версия пакета: версия pult + время сборки (до секунды) и коммит (каждая новая сборка — «новее» прежней).
# Нужны dpkg-deb и objdump (binutils); fakeroot не нужен: владельцев файлов задаёт --root-owner-group.
set -eu

BIN=${1:?путь к бинарнику pult}
OUT=${2:?каталог для пакета}
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)

base=$("$BIN" version | awk '{print $2}')
[ -n "$base" ] || { echo "$BIN version ничего не вернул" >&2; exit 1; }
stamp=$(date -u +%Y%m%d%H%M%S)
sha=$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo local)
VERSION=${PULT_DEB_VERSION:-$base+$stamp.$sha}
# Версия идёт в sed и в имя файла: только символы версии Debian.
printf '%s' "$VERSION" | grep -Eq '^[0-9][0-9A-Za-z.+~:-]*$' || { echo "недопустимая версия пакета: $VERSION" >&2; exit 1; }
# Демо-сборка (поддельный домен, общий пароль) в пакет не попадает.
if "$BIN" demo --help >/dev/null 2>&1; then
    echo "$BIN собран с --features demo — такой бинарник упаковывать нельзя" >&2
    exit 1
fi

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
install -d "$ROOT/DEBIAN" "$ROOT/opt/pult/bin" "$ROOT/usr/lib/systemd/system" "$ROOT/usr/share/doc/pult"
install -m 0755 "$BIN" "$ROOT/opt/pult/bin/pult"
install -m 0644 "$REPO/deploy/systemd/pult.service" "$ROOT/usr/lib/systemd/system/pult.service"
install -m 0644 "$REPO/LICENSE" "$ROOT/usr/share/doc/pult/copyright"
install -m 0644 "$REPO/docs/install.md" "$ROOT/usr/share/doc/pult/install.md"
for s in preinst postinst prerm postrm; do
    install -m 0755 "$HERE/debian/$s" "$ROOT/DEBIAN/$s"
done
size=$(du -sk --exclude=DEBIAN "$ROOT" | cut -f1)
# Наименьшая подходящая glibc — по символам бинарника: собран на новой системе — на старой не ставится.
glibc=$(objdump -T "$BIN" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n 1)
[ -n "$glibc" ] || { echo "не удалось определить версию glibc (нужен objdump из binutils)" >&2; exit 1; }
sed -e "s/@VERSION@/$VERSION/" -e "s/@SIZE@/$size/" -e "s/@GLIBC@/$glibc/" "$HERE/debian/control" >"$ROOT/DEBIAN/control"

mkdir -p "$OUT"
deb="$OUT/pult_${VERSION}_amd64.deb"
dpkg-deb --root-owner-group -Zxz --build "$ROOT" "$deb" >/dev/null
( cd "$OUT" && sha256sum "$(basename "$deb")" >"$(basename "$deb").sha256" )
echo "$deb"
