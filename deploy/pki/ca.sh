#!/bin/sh
# Небольшой корневой центр сертификации для внутренних служб (docs/ad-setup.md, раздел 2).
#
#   PKI_DOMAIN=corp.example.local PKI_NET=10.0.0.0/16 PKI_CA_NAME="Example Internal Root CA" \
#   deploy/pki/ca.sh init DIR            — создать CA в каталоге DIR (флешка): ключ шифруется фразой-паролем
#   deploy/pki/ca.sh sign DIR REQ.csr    — подписать запрос сервера; сертификат — рядом с запросом (.crt)
#   deploy/pki/ca.sh revoke DIR FILE.crt — отозвать сертификат и выпустить новый CRL
#   deploy/pki/ca.sh crl DIR             — выпустить CRL заново (раз в год, до nextUpdate)
#   deploy/pki/ca.sh show FILE.crt       — показать сертификат
#
# Домен, сеть и имя CA задаются только при init. Остальные команды берут их из самого сертификата CA
# (Name Constraints и CN), поэтому проверка имён при подписи всегда совпадает с ограничениями CA.
#
# Безопасность:
# - Ключ CA создаётся сразу зашифрованным (AES-256); фразу-пароль вводит администратор, она нигде не сохраняется.
# - Name Constraints: CA может подписывать только PKI_DOMAIN с поддоменами и сеть PKI_NET — утёкший ключ
#   не позволит подделать чужой сайт.
# - Ключи серверов сюда не попадают: подписывается только запрос (CSR), созданный на самом сервере.
#   Из запроса берутся только имена (SAN); назначение, срок и ограничения задаёт CA.
# - Список отзыва (CRL) публикуется в AD (`certutil -dspublish`), адрес — в каждом сертификате сервера:
#   без него Windows (Schannel) не может проверить отзыв и строгие клиенты отказывают.
# Требуется OpenSSL 3 (есть в Git for Windows и Debian).
set -eu

CDP_CONTAINER="${PKI_CDP_CONTAINER:-pki}"
CA_DAYS=3650
LEAF_DAYS=825
CRL_DAYS=400
# Только для автоматических тестов скрипта (deploy/pki/test-ca.sh): передать фразу-пароль не из терминала.
PASS_OUT="${PKI_TEST_PASSOUT:-}"
PASS_IN="${PKI_TEST_PASSIN:-}"

die() { echo "ОШИБКА: $*" >&2; exit 1; }
# Git Bash: аргумент «/CN=…» не должен превращаться в путь Windows (остальные пути — должны).
export MSYS2_ARG_CONV_EXCL='/CN='

# IPv4 -> число; пусто, если это не IPv4.
ip2int() {
	echo "$1" | awk -F. 'NF == 4 { for (i = 1; i <= 4; i++) if ($i !~ /^[0-9]+$/ || $i > 255) exit; printf "%.0f\n", (($1 * 256 + $2) * 256 + $3) * 256 + $4 }'
}
int2ip() { echo "$1" | awk '{ printf "%d.%d.%d.%d\n", int($1 / 16777216) % 256, int($1 / 65536) % 256, int($1 / 256) % 256, $1 % 256 }'; }

# DOMAIN, NET_ADDR, NET_MASK (числа), CA_NAME — из сертификата CA.
load_ca() {
	[ -f "$1/ca.crt" ] && [ -f "$1/ca.key" ] && [ -f "$1/index.txt" ] || die "в $1 нет CA — сначала init"
	nc="$(openssl x509 -in "$1/ca.crt" -noout -ext nameConstraints 2>/dev/null | tr -d ' ')"
	DOMAIN="$(echo "$nc" | sed -n 's/^DNS:\([^.].*\)$/\1/p' | head -n 1)"
	net="$(echo "$nc" | sed -n 's/^IP:\([0-9.]*\/[0-9.]*\)$/\1/p' | head -n 1)"
	[ -n "$DOMAIN" ] && [ -n "$net" ] || die "в $1/ca.crt нет Name Constraints (DNS и IPv4) — такой CA скрипт не обслуживает"
	NET_ADDR="$(ip2int "${net%/*}")"
	NET_MASK="$(ip2int "${net#*/}")"
	CA_NAME="$(openssl x509 -in "$1/ca.crt" -noout -subject -nameopt multiline | sed -n 's/^ *commonName *= *//p')"
	[ -n "$CA_NAME" ] || die "в $1/ca.crt нет CN"
}

# IP внутри сети CA? Побитовое И без gawk: сравниваем адреса, отбросив младшие биты по размеру сети.
in_net() {
	ip="$(ip2int "$1")"
	[ -n "$ip" ] || return 1
	awk -v ip="$ip" -v net="$NET_ADDR" -v mask="$NET_MASK" 'BEGIN {
		size = 4294967296 - mask
		exit !(int(ip / size) == int(net / size))
	}'
}

# Адрес CRL в AD — там же, где его публикует AD CS: CN=<CA>,CN=<контейнер>,CN=CDP,CN=Public Key Services,…
ldap_cdp() {
	dc="$(echo "$DOMAIN" | sed 's/^/DC=/; s/\./,DC=/g')"
	cn="$(echo "$CA_NAME" | sed 's/ /%20/g')"
	echo "ldap:///CN=$cn,CN=$CDP_CONTAINER,CN=CDP,CN=Public%20Key%20Services,CN=Services,CN=Configuration,$dc?certificateRevocationList?base?objectClass=cRLDistributionPoint"  # SAFE: адрес CRL в AD (как у AD CS), не подключение Pult к LDAP
}

# Расширение crlDistributionPoints в DER (hex). Через `URI:` нельзя: OpenSSL делит значение по запятым,
# а в DN адреса LDAP запятые есть. Структура: SEQUENCE { SEQUENCE { [0] { [0] { [6] URI } } } }.
cdp_der_hex() {
	tmp="$(mktemp -d)"
	printf 'asn1 = SEQUENCE:cdps\n[cdps]\np = SEQUENCE:point\n[point]\ndpn = EXPLICIT:0C,IMPLICIT:0C,SEQUENCE:names\n[names]\nuri = IMPLICIT:6,IA5STRING:%s\n' \
		"$1" >"$tmp/cdp.cnf"
	openssl asn1parse -genconf "$tmp/cdp.cnf" -out "$tmp/cdp.der" >/dev/null
	od -An -tx1 -v "$tmp/cdp.der" | tr -d ' \n'
	rm -rf "$tmp"
}

# openssl ca работает с путями относительно каталога CA.
in_ca() { (cd "$1" && shift && "$@"); }

gencrl() {
	in_ca "$1" openssl ca -config ca.cnf -gencrl $PASS_IN -out ca.crl.pem
	openssl crl -in "$1/ca.crl.pem" -outform DER -out "$1/ca.crl"
	echo "CRL: $1/ca.crl (действует до $(openssl crl -in "$1/ca.crl.pem" -noout -nextupdate | cut -d= -f2))."
	echo "Опубликовать в AD (Enterprise Admins): certutil -dspublish -f ca.crl $CDP_CONTAINER \"$CA_NAME\""
}

cmd="${1:-}"
case "$cmd" in
init)
	dir="${2:?каталог CA}"
	[ -e "$dir/ca.key" ] && die "в $dir уже есть ca.key — CA не пересоздаю"
	DOMAIN="${PKI_DOMAIN:?PKI_DOMAIN — домен AD, например corp.example.local}"
	cidr="${PKI_NET:?PKI_NET — сеть серверов IPv4, например 10.0.0.0/16}"
	CA_NAME="${PKI_CA_NAME:?PKI_CA_NAME — имя CA, например Example Internal Root CA}"
	echo "$DOMAIN" | grep -Eq '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$' ||
		die "PKI_DOMAIN «$DOMAIN» — нужен домен в нижнем регистре"
	echo "$CA_NAME" | grep -Eq '^[A-Za-z0-9 ._-]+$' || die "PKI_CA_NAME — только латиница, цифры, пробел и ._-"
	bits="${cidr#*/}"
	addr="$(ip2int "${cidr%/*}")"
	{ [ "$bits" != "$cidr" ] && [ -n "$addr" ] && echo "$bits" | grep -Eq '^([89]|[12][0-9]|3[0-2])$'; } ||
		die "PKI_NET «$cidr» — нужна сеть IPv4 вида 10.0.0.0/16 (от /8 до /32)"
	size="$(awk -v b="$bits" 'BEGIN { printf "%.0f", 2 ^ (32 - b) }')"
	[ "$(awk -v a="$addr" -v s="$size" 'BEGIN { printf "%.0f", a % s }')" = 0 ] || die "PKI_NET «$cidr» — адрес не совпадает с началом сети"
	NET="$(int2ip "$addr")/$(int2ip "$(awk -v s="$size" 'BEGIN { printf "%.0f", 4294967296 - s }')")"
	ORG_LINE=""
	[ -n "${PKI_ORG:-}" ] && ORG_LINE="O = $PKI_ORG"
	mkdir -p "$dir/issued"
	umask 077
	cat >"$dir/ca.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = v3_ca
[dn]
CN = $CA_NAME
$ORG_LINE
[v3_ca]
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
nameConstraints = critical, permitted;DNS:$DOMAIN, permitted;DNS:.$DOMAIN, permitted;IP:$NET

[ca]
default_ca = internal
[internal]
database = index.txt
new_certs_dir = issued
serial = serial
crlnumber = crlnumber
certificate = ca.crt
private_key = ca.key
default_md = sha384
default_days = $LEAF_DAYS
default_crl_days = $CRL_DAYS
policy = policy_cn
copy_extensions = none
unique_subject = no
email_in_dn = no
[policy_cn]
commonName = supplied
[crl_ext]
authorityKeyIdentifier = keyid
EOF
	: >"$dir/index.txt"
	echo 1000 >"$dir/serial"
	echo 1000 >"$dir/crlnumber"
	echo "CA «$CA_NAME»: только $DOMAIN (с поддоменами) и $cidr."
	echo "Фраза-пароль для ключа CA (не короче 16 символов; без неё ключ бесполезен — сохраните в менеджере паролей):"
	openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-384 -aes-256-cbc $PASS_OUT -out "$dir/ca.key"
	echo "Фраза-пароль — для подписи сертификата CA:"
	openssl req -new -x509 -config "$dir/ca.cnf" -key "$dir/ca.key" $PASS_IN -sha384 -days "$CA_DAYS" -out "$dir/ca.crt"
	chmod 0400 "$dir/ca.key"
	echo "Фраза-пароль — для первого списка отзыва (CRL):"
	gencrl "$dir"
	echo
	echo "CA создан: $dir/ca.crt (публичный — его раздают), $dir/ca.key (секрет — держать офлайн)."
	echo "Опубликовать в AD как доверенный корень (Enterprise Admins): certutil -dspublish -f ca.crt RootCA"
	openssl x509 -in "$dir/ca.crt" -noout -subject -enddate -fingerprint -sha256
	;;
sign)
	dir="${2:?каталог CA}"
	csr="${3:?файл запроса .csr}"
	load_ca "$dir"
	CDP="${PKI_CDP:-$(ldap_cdp)}"
	openssl req -in "$csr" -noout -verify 2>/dev/null || die "$csr — повреждён или подпись запроса неверна"
	# Имена — только из SAN запроса и только в пределах домена и сети CA.
	san="$(openssl req -in "$csr" -noout -text 2>/dev/null | grep -A1 "Subject Alternative Name" | tail -n 1 |
		tr -d ' ' | tr ',' '\n' | grep -E '^(DNS|IPAddress):' || true)"
	[ -n "$san" ] || die "в запросе нет subjectAltName — без имён сертификат бесполезен"
	ext=""
	for item in $san; do
		kind="${item%%:*}"
		value="$(echo "${item#*:}" | tr 'A-Z' 'a-z')"
		case "$kind:$value" in
		DNS:"$DOMAIN" | DNS:*."$DOMAIN") ext="$ext${ext:+,}DNS:$value" ;;
		IPAddress:*)
			in_net "$value" || die "адрес $value вне сети CA — не подписываю"
			ext="$ext${ext:+,}IP:$value"
			;;
		*) die "имя $value вне $DOMAIN — CA такое не подписывает" ;;
		esac
	done
	cn="$(echo "$ext" | cut -d, -f1 | cut -d: -f2)"
	csr_abs="$(cd "$(dirname "$csr")" && pwd)/$(basename "$csr")"
	out="${csr_abs%.csr}.crt"
	cat >"$dir/server.ext" <<EOF
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = $ext
authorityKeyIdentifier = keyid
subjectKeyIdentifier = hash
crlDistributionPoints = DER:$(cdp_der_hex "$CDP")
EOF
	echo "Подписываю для: $ext (срок $LEAF_DAYS дней). Фраза-пароль ключа CA:"
	in_ca "$dir" openssl ca -config ca.cnf -batch -notext $PASS_IN -in "$csr_abs" -out "$out" \
		-extfile server.ext -subj "/CN=$cn"
	rm -f "$dir/server.ext"
	cat "$out" "$dir/ca.crt" >"${out%.crt}.chain.crt"
	openssl verify -CAfile "$dir/ca.crt" "$out"
	echo "Сертификат: $out, цепочка для сервера: ${out%.crt}.chain.crt"
	;;
revoke)
	dir="${2:?каталог CA}"
	crt="${3:?файл сертификата .crt}"
	load_ca "$dir"
	crt_abs="$(cd "$(dirname "$crt")" && pwd)/$(basename "$crt")"
	echo "Отзываю $(openssl x509 -in "$crt_abs" -noout -subject -serial | paste -sd' '). Фраза-пароль ключа CA:"
	in_ca "$dir" openssl ca -config ca.cnf $PASS_IN -revoke "$crt_abs"
	echo "Новый CRL. Фраза-пароль ключа CA:"
	gencrl "$dir"
	;;
crl)
	dir="${2:?каталог CA}"
	load_ca "$dir"
	echo "Фраза-пароль ключа CA:"
	gencrl "$dir"
	;;
show)
	openssl x509 -in "${2:?файл .crt}" -noout -subject -issuer -dates -serial \
		-ext subjectAltName,extendedKeyUsage,basicConstraints,nameConstraints,crlDistributionPoints -fingerprint -sha256
	;;
*)
	echo "использование: PKI_DOMAIN=… PKI_NET=… PKI_CA_NAME=… $0 init DIR | sign DIR REQ.csr | revoke DIR FILE.crt | crl DIR | show FILE.crt" >&2
	exit 2
	;;
esac
