# Приложение: подготовка домена и сети для Pult

> **Для обычной установки это не нужно** — достаточно [docs/install.md](install.md). Здесь необязательное: свой центр сертификации, если в домене нет AD CS, Kerberos (вход через домен без пароля; включается в «Безопасность» после настройки по этому документу), зона «Местная интрасеть». Скрипты — готовые помощники для этих случаев, всё можно сделать и стандартными средствами Windows.

Что делает администратор домена. Пароли он вводит сам — в окне Windows или в запросе команды, никогда в файлах репозитория.

В примерах: домен `corp.example.local` (`CORP`), сеть серверов `10.0.0.0/16`, портал `pult.corp.example.local` (`10.0.0.32`), SQL Server со старым справочником `sql01.corp.example.local` (`10.0.0.5`), администратор `CORP\admin`. Подставьте свои значения. Записывать их в репозиторий не нужно: скрипты принимают их параметрами. Конкретную инсталляцию лучше описывать в отдельном приватном месте.

## 1. Сеть: постоянный адрес и имя сервера

- **DHCP** — статическая аренда для сервера Pult, либо статический адрес.
- **DNS** — запись A и, если есть обратная зона, PTR. Если на ПК администратора нет модуля DnsServer, через удалённый PowerShell на DC (пароль спросит окно Windows):

```powershell
Invoke-Command -ComputerName dc01 -Credential CORP\admin -ScriptBlock {
  Add-DnsServerResourceRecordA -ZoneName corp.example.local -Name pult -IPv4Address 10.0.0.32 -CreatePtr
}
```

Проверка с любого ПК: `Resolve-DnsName pult.corp.example.local` → `10.0.0.32`.

## 2. Сертификаты — свой корневой CA

Если в домене нет AD CS, подойдёт небольшой корневой CA — `deploy/pki/ca.sh` (OpenSSL; на Windows — через bash из Git for Windows):

- ключ CA — EC P-384, сразу зашифрован фразой-паролем администратора, хранится **офлайн** (флешка/сейф); сертификат CA — 10 лет;
- **Name Constraints**: CA подписывает только свой домен (с поддоменами) и свою сеть — утёкший ключ не позволит подделать чужой сайт. Проверяется `deploy/pki/test-ca.sh`;
- ключи серверов создаются на самих серверах, CA подписывает только запросы (CSR); срок сертификата сервера — 825 дней;
- **CRL публикуется в AD** (`CN=<CA>,CN=pki,CN=CDP,CN=Public Key Services,…`), адрес — в каждом сертификате сервера: без него Windows (Schannel) не может проверить отзыв и строгие клиенты отказывают. CRL действует 400 дней — **выпускать заново раз в год** (`ca.sh crl`) и публиковать.

Домен, сеть и имя CA задаются один раз при `init`; `sign`, `crl` и `revoke` берут их из сертификата CA.

```bash
# 1. Создать CA (фраза-пароль — 4 раза: задать, повторить, подписать CA, подписать первый CRL)
PKI_DOMAIN=corp.example.local PKI_NET=10.0.0.0/16 PKI_CA_NAME="CORP Internal Root CA" PKI_ORG=CORP \
  deploy/pki/ca.sh init /e/pki/ca
# 2. Подписать запрос сервера Pult (создан на сервере, ключ остался там)
deploy/pki/ca.sh sign /e/pki/ca /e/pki/requests/pult.csr
```

```powershell
# 3. Опубликовать в AD (Enterprise Admins): корень — доверенным для всех ПК домена, CRL — для проверки отзыва
runas /user:CORP\admin "certutil -dspublish -f E:\pki\ca\ca.crt RootCA"
runas /user:CORP\admin "certutil -dspublish -f E:\pki\ca\ca.crl pki \"CORP Internal Root CA\""
```

ПК домена подхватят корень при следующем обновлении групповой политики (`gpupdate /force` или в течение 90 минут). Firefox по умолчанию использует своё хранилище — включить политику `Certificates → ImportEnterpriseRoots`. `ca.crt` и `ca.crl` — публичные; каталог с `ca.key` держать офлайн.

**Сертификат SQL Server** (если Pult импортирует данные из SQL Server; ключ RSA — старые версии SQL Server не принимают ECDSA). Запрос создаётся на самом сервере (ключ в хранилище компьютера, неэкспортируемый), подписывается `ca.sh sign`, принимается `certreq -accept`. Затем службе SQL Server выдаётся право чтения ключа, сертификат прописывается в настройки, SQL Server перезапускается. После этого клиентам не нужен `TrustServerCertificate=True`. Всё это делает `deploy/pki/sql-server-cert.ps1`:

```powershell
$p = @{ Server = "sql01"; Fqdn = "sql01.corp.example.local"; Ip = "10.0.0.5"; Admin = "CORP\admin"; Dir = "C:\pki\requests" }
.\deploy\pki\sql-server-cert.ps1 -Stage Request @p              # ключ и CSR на сервере
#   deploy/pki/ca.sh sign /e/pki/ca /c/pki/requests/sql01-sql.csr
.\deploy\pki\sql-server-cert.ps1 -Stage Install @p -RestartSql  # клиенты SQL — пауза на секунды
# откат: -Stage Rollback @p -RestartSql
```

Журнал — `sql-server-cert.log` в каталоге `-Dir`.

**Сертификат LDAPS для контроллеров домена** (если в домене нет AD CS). Без сертификата контроллер сбрасывает соединения на порт 636, и Pult не может на него переключиться. Сертификат нужен **каждому** контроллеру из списка Pult. Запрос создаётся на контроллере (ключ RSA в хранилище компьютера, неэкспортируемый; SAN — полное имя и IP), подписывается `ca.sh sign`, принимается `certreq -accept`. AD подхватывает его без перезагрузки: скрипт выполняет операцию `renewServerCertificate`. Если на контроллере уже стоит сертификат SQL Server от этого CA с его именем, AD возьмёт для LDAPS и его. `deploy/pki/dc-ldaps-cert.ps1`:

```powershell
$p = @{ Server = "dc2"; Fqdn = "dc2.corp.example.local"; Ip = "10.0.0.2"; Admin = "CORP\admin"; Dir = "C:\pki\requests" }
.\deploy\pki\dc-ldaps-cert.ps1 -Stage Request @p    # ключ и CSR на контроллере
#   deploy/pki/ca.sh sign /e/pki/ca /c/pki/requests/dc2-ldaps.csr
.\deploy\pki\dc-ldaps-cert.ps1 -Stage Install @p    # без перезагрузки и перезапуска служб
# откат: -Stage Rollback @p (удаляет установленный сертификат)
```

Проверка с сервера Pult: `openssl s_client -connect dc2.corp.example.local:636 -verify_return_error </dev/null` — `Verify return code: 0 (ok)`; в админке Pult «AD → Проверить подключение» — все контроллеры зелёные.

## 3. Служебные учётки и SPN

`deploy/ad/pult-service-accounts.ps1` создаёт OU `Service Accounts` и три учётки (повторный запуск безопасен; пароли учёток вводятся скрыто, длинные случайные):

| Учётка | Зачем |
|---|---|
| `svc-pult-http` | вход через домен (Kerberos): SPN `HTTP/<имя портала>`, только AES256 |
| `svc-pult-ldap` | чтение каталога AD; обычный пользователь без дополнительных прав |
| `svc-pult-sql` | чтение старого справочника в SQL Server (Windows-вход) |

```powershell
powershell -ExecutionPolicy Bypass -File deploy\ad\pult-service-accounts.ps1 -Admin CORP\admin -SpnHost pult.corp.example.local
```

**Keytab для HTTP** — без `ktpass`, учётка не меняется (на сервере Pult, `ktutil` из MIT Kerberos; пароль `svc-pult-http` вводится в запросе):

```
ktutil
addent -password -p HTTP/pult.corp.example.local@CORP.EXAMPLE.LOCAL -k 2 -e aes256-cts-hmac-sha1-96 -s CORP.EXAMPLE.LOCALsvc-pult-http
addent -password -p svc-pult-http@CORP.EXAMPLE.LOCAL -k 2 -e aes256-cts-hmac-sha1-96 -s CORP.EXAMPLE.LOCALsvc-pult-http
wkt /etc/pult/pult-http.keytab
quit
```

Соль AD для учётки пользователя — `REALM` + `sAMAccountName`; `-k` — текущий kvno (`Get-ADUser svc-pult-http -Properties msDS-KeyVersionNumber`). Если пароль учётки сменят, kvno вырастет — keytab нужно пересоздать. Keytab — секрет, равный паролю: `root:pult 0640`, путь — в `KRB5_KTNAME`. Проверка: `kinit -k -t /etc/pult/pult-http.keytab HTTP/pult.corp.example.local` и `kvno`.

**Права в SQL Server** (под администратором SQL), только чтение:

```sql
CREATE LOGIN [CORP\svc-pult-sql] FROM WINDOWS;
USE PhonebookDB;
CREATE USER [CORP\svc-pult-sql] FOR LOGIN [CORP\svc-pult-sql];
ALTER ROLE db_datareader ADD MEMBER [CORP\svc-pult-sql];
```

## 4. Kerberos: зона «Местная интрасеть»

Без неё браузер не отправит билет Kerberos. `deploy/ad/pult-intranet-zone-gpo.ps1` создаёт GPO «Pult — местная интрасеть» (Конфигурация компьютера → «Список назначений зоны для веб-сайтов»: адрес портала → `1`) и привязывает её к домену; повторный запуск безопасен, `-Remove` — отменить:

```powershell
.\deploy\ad\pult-intranet-zone-gpo.ps1 -Sites https://pult.corp.example.local
gpupdate /target:computer /force   # на ПК, где проверяем, — чтобы не ждать 90 минут
```

Браузер, открытый до применения GPO, нужно перезапустить. По IP-адресу Kerberos не работает (браузер переходит на NTLM, Pult его отклоняет) — только по DNS-имени.

## 5. Риски, которые стоит проверить

- **SQL Server на контроллере домена.** Компрометация SQL Server (или учётки с правами на него) там равна компрометации домена. Если справочник нужен только для разового импорта в Pult, SQL Server с DC после него лучше убрать.
- **Постоянное членство в Schema Admins / Enterprise Admins.** Эти группы обычно держат пустыми и добавляют учётку только на время операции.
