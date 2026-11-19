#!/bin/bash
# Первый запуск: ROLE=primary — создать домен; ROLE=replica — присоединиться к PRIMARY_DC.
# Сертификат CA, которым Samba подписывает LDAPS, копируется в /export/<hostname>-ca.pem.
set -euo pipefail

: "${REALM:?REALM, например PULT.TEST}"
: "${DOMAIN:?DOMAIN, например PULT}"
: "${ADMIN_PASSWORD:?ADMIN_PASSWORD}"
ROLE="${ROLE:-primary}"
HOST="$(hostname -s)"

if [ ! -f /var/lib/samba/private/sam.ldb ]; then
  rm -f /etc/samba/smb.conf
  if [ "$ROLE" = "primary" ]; then
    echo "[$HOST] создаю домен $REALM"
    samba-tool domain provision \
      --realm="$REALM" --domain="$DOMAIN" --server-role=dc \
      --dns-backend=SAMBA_INTERNAL --adminpass="$ADMIN_PASSWORD" \
      --option="dns forwarder = ${DNS_FORWARDER:-1.1.1.1}"
  else
    : "${PRIMARY_DC:?PRIMARY_DC для реплики}"
    echo "[$HOST] жду $PRIMARY_DC"
    until ldbsearch -H "ldap://$PRIMARY_DC" -s base -b "" >/dev/null 2>&1; do sleep 3; done  # SAFE: анонимный RootDSE внутри сети compose
    echo "[$HOST] присоединяюсь к $REALM как DC"
    samba-tool domain join "$REALM" DC \
      -U"Administrator%$ADMIN_PASSWORD" --server="$PRIMARY_DC" \
      --dns-backend=SAMBA_INTERNAL \
      --option="dns forwarder = ${DNS_FORWARDER:-1.1.1.1}"
  fi
  # smb.conf лежит вне тома: без копии новый контейнер на старом томе не запустится.
  cp /etc/samba/smb.conf /var/lib/samba/smb.conf
fi
[ -f /etc/samba/smb.conf ] || cp /var/lib/samba/smb.conf /etc/samba/smb.conf
cp /var/lib/samba/private/krb5.conf /etc/krb5.conf

# Свой CA и сертификат DC. Самоподписанный сертификат Samba содержит имя только в CN, а rustls
# (как и современные браузеры) проверяет имя по subjectAltName. Samba берёт готовые файлы
# по стандартным путям tls/{key,cert,ca}.pem и свои не генерирует.
TLS=/var/lib/samba/private/tls
FQDN="${HOST}.${REALM,,}"
if [ ! -f "$TLS/key.pem" ]; then
  mkdir -p "$TLS"
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Pult test CA ${HOST}" \
    -keyout "$TLS/ca-key.pem" -out "$TLS/ca.pem" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign"
  openssl req -newkey rsa:2048 -nodes -subj "/CN=${FQDN}" -keyout "$TLS/key.pem" -out "$TLS/req.csr"
  printf 'subjectAltName=DNS:%s,DNS:%s\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature,keyEncipherment\nbasicConstraints=CA:FALSE\n' \
    "$FQDN" "$HOST" >"$TLS/ext.cnf"
  openssl x509 -req -in "$TLS/req.csr" -CA "$TLS/ca.pem" -CAkey "$TLS/ca-key.pem" -CAcreateserial \
    -days 825 -extfile "$TLS/ext.cnf" -out "$TLS/cert.pem"
  chmod 600 "$TLS/key.pem" "$TLS/ca-key.pem"
  rm -f "$TLS/req.csr" "$TLS/ext.cnf"
fi
cp "$TLS/ca.pem" "/export/${HOST}-ca.pem"

if [ "$ROLE" = "primary" ]; then /usr/local/bin/seed-users.sh & fi

exec samba --foreground --no-process-group
