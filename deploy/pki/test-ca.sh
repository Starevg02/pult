#!/bin/sh
# Самопроверка deploy/pki/ca.sh на одноразовом CA во временном каталоге (настоящий CA не трогается).
#   deploy/pki/test-ca.sh
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
t="$(mktemp -d)"
trap 'rm -rf "$t"' EXIT
export MSYS2_ARG_CONV_EXCL='/CN='
export PKI_TEST_PASSOUT="-pass pass:test-only-passphrase" PKI_TEST_PASSIN="-passin pass:test-only-passphrase"
ok() { echo "✅ $*"; }
fail() { echo "❌ $*"; exit 1; }

if sh "$here/ca.sh" init "$t/noparams" >/dev/null 2>&1; then fail "init без PKI_DOMAIN/PKI_NET"; else ok "init без домена и сети отказывает"; fi
if PKI_DOMAIN=corp.example.test PKI_NET=10.20.1.0/16 PKI_CA_NAME="Test Root" sh "$here/ca.sh" init "$t/badnet" >/dev/null 2>&1; then
	fail "принята сеть 10.20.1.0/16"
else ok "сеть, не выровненная по маске, отклонена"; fi

PKI_DOMAIN=corp.example.test PKI_NET=10.20.0.0/16 PKI_CA_NAME="Test Internal Root CA" sh "$here/ca.sh" init "$t/ca" >/dev/null
grep -q "ENCRYPTED PRIVATE KEY" "$t/ca/ca.key" && ok "ключ CA зашифрован" || fail "ключ CA не зашифрован"
nc="$(openssl x509 -in "$t/ca/ca.crt" -noout -ext nameConstraints)"
echo "$nc" | grep -q "DNS:.corp.example.test" && echo "$nc" | grep -q "IP:10.20.0.0/255.255.0.0" && ok "Name Constraints в CA" || fail "нет Name Constraints"

csr() { # имя, SAN
	openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$t/$1.key" -subj "/CN=$1" \
		-addext "subjectAltName=$2" -out "$t/$1.csr" 2>/dev/null
}
# Дальше переменных окружения нет: домен и сеть sign берёт из сертификата CA.
csr good "DNS:pult.corp.example.test,IP:10.20.0.32"
sh "$here/ca.sh" sign "$t/ca" "$t/good.csr" >/dev/null
openssl verify -CAfile "$t/ca/ca.crt" "$t/good.crt" >/dev/null && ok "сертификат сервера подписан и проверяется" || fail "проверка цепочки"
openssl x509 -in "$t/good.crt" -noout -ext extendedKeyUsage | grep -q "Server Authentication" && ok "назначение — serverAuth" || fail "нет serverAuth"
openssl x509 -in "$t/good.crt" -noout -ext basicConstraints | grep -q "CA:FALSE" && ok "сертификат сервера не может быть CA" || fail "CA:TRUE у сервера"

reject() { # имя, SAN, описание
	csr "$1" "$2"
	if sh "$here/ca.sh" sign "$t/ca" "$t/$1.csr" >/dev/null 2>&1; then fail "подписано: $3"; else ok "отклонено: $3"; fi
}
reject evil "DNS:bank.example.com" "чужой домен"
reject suffix "DNS:evilcorp.example.test" "домен с тем же окончанием без точки"
reject ip "DNS:pult.corp.example.test,IP:10.21.0.1" "адрес вне сети CA"
# Даже если подписать в обход скрипта — Name Constraints не дадут такому сертификату пройти проверку.
printf 'subjectAltName=DNS:bank.example.com\n' >"$t/evil.ext"
openssl x509 -req -in "$t/evil.csr" -CA "$t/ca/ca.crt" -CAkey "$t/ca/ca.key" $PKI_TEST_PASSIN -CAcreateserial \
	-days 1 -extfile "$t/evil.ext" -out "$t/evil.crt" 2>/dev/null
if openssl verify -CAfile "$t/ca/ca.crt" "$t/evil.crt" >/dev/null 2>&1; then fail "Name Constraints не сработали"; else ok "подпись в обход скрипта отвергается проверкой (Name Constraints)"; fi

openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$t/nosan.key" -subj "/CN=x" -out "$t/nosan.csr" 2>/dev/null
if sh "$here/ca.sh" sign "$t/ca" "$t/nosan.csr" >/dev/null 2>&1; then fail "подписан запрос без SAN"; else ok "запрос без имён отклонён"; fi

openssl x509 -in "$t/good.crt" -noout -ext crlDistributionPoints |
	grep -q "ldap:///CN=Test%20Internal%20Root%20CA,CN=pki,CN=CDP,CN=Public%20Key%20Services,CN=Services,CN=Configuration,DC=corp,DC=example,DC=test" && # SAFE: проверка адреса CRL, не подключение к LDAP
	ok "адрес CRL (AD) в сертификате сервера" || fail "нет адреса CRL"
[ -f "$t/ca/ca.crl" ] && openssl crl -in "$t/ca/ca.crl" -inform DER -noout -nextupdate >/dev/null && ok "CRL выпущен (DER для certutil)" || fail "нет CRL"
sh "$here/ca.sh" revoke "$t/ca" "$t/good.crt" >/dev/null
serial="$(openssl x509 -in "$t/good.crt" -noout -serial | cut -d= -f2)"
openssl crl -in "$t/ca/ca.crl" -inform DER -noout -text | grep -q "Serial Number: $serial" && ok "отозванный сертификат в CRL" || fail "отзыва нет в CRL"
openssl verify -crl_check -CRLfile "$t/ca/ca.crl.pem" -CAfile "$t/ca/ca.crt" "$t/good.crt" >/dev/null 2>&1 && fail "отозванный проходит проверку" || ok "отозванный сертификат не проходит проверку с CRL"
echo "Все проверки CA пройдены"
