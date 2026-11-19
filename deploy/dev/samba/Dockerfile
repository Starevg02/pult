# Тестовый контроллер домена Samba AD для разработки Pult (план 21, «Тестовый стенд»).
# Только для compose.dev.yml. Не использовать в продакшене.
FROM debian:bookworm-slim

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      samba samba-ad-dc samba-ad-provision winbind krb5-user ldb-tools dnsutils ca-certificates openssl \
 && rm -rf /var/lib/apt/lists/* \
 && rm -f /etc/samba/smb.conf

COPY entrypoint.sh seed-users.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/seed-users.sh

# 53 — DNS, 88 — Kerberos, 389 — LDAP/StartTLS, 636 — LDAPS
EXPOSE 53/udp 53/tcp 88/udp 88/tcp 389 636
VOLUME ["/var/lib/samba", "/export"]
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
