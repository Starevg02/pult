# Deploying Pult on a server

This guide takes you from an empty virtual machine to a working portal. It usually takes 30–60 minutes.
Windows scripts and config edits are not needed for a basic installation: the package does the work, and the rest
is done in the browser wizard. Russian version: [install.md](install.md).

After installation, see [setup and operation](guide.md) (Russian). For how the system works internally, see
[architecture.md](architecture.md) (Russian). For the security model, see [security.md](security.md) (Russian).

## Contents

1. [How it works](#1-how-it-works)
2. [What to prepare in advance](#2-what-to-prepare-in-advance)
3. [Where to get the installer](#3-where-to-get-the-installer)
4. [Installation](#4-installation)
5. [First-run wizard](#5-first-run-wizard)
6. [Portal certificate](#6-portal-certificate)
7. [Optional steps](#7-optional-steps)
8. [Backups and restore](#8-backups-and-restore)
9. [Update](#9-update)
10. [Removal](#10-removal)
11. [Docker instead of the package](#11-docker-instead-of-the-package)
12. [Database on another server](#12-database-on-another-server)
13. [Server parameters](#13-server-parameters)
14. [pult commands](#14-pult-commands)
15. [Troubleshooting](#15-troubleshooting)

## 1. How it works

Pult is one program (`/opt/pult/bin/pult`) with a built-in web interface and a PostgreSQL database. The program runs
as the `pult` service, serves HTTPS on port 443, and connects to domain controllers only over a secure channel
(LDAPS or StartTLS). Employees sign in with their Windows password. The portal's service account can only read the
AD directory.

```
 employee browsers ──HTTPS 443──►  Pult server  ──LDAPS 636 / StartTLS 389──►  domain controllers
 agents on PCs (optional) ─HTTPS 443─►  (pult service  ──SMTP──►  mail server (optional)
                                         + PostgreSQL)
```

When installed, the `.deb` package installs PostgreSQL itself, creates the database, the encryption key and a
temporary certificate, and starts the service. When updating, it makes a backup and, if the backup or the database
migration fails, brings back the previous version.

## 2. What to prepare in advance

### Server

| What | Requirement | Why |
|---|---|---|
| System | Debian 13 or Ubuntu 24.04 or newer, x86-64 (amd64), systemd | The package is built and tested for these |
| Resources | 2 vCPU, at least 2 GB of memory (4 GB preferred), at least 20 GB of disk | The portal and PostgreSQL run on one server; 14 nightly backups |
| Address | Fixed IP (static or a DHCP reservation) | The portal's DNS record points to this address |
| Name | Full DNS name, for example `pult.corp.example.test`, an A record in internal DNS | The certificate is issued for this name, and the Kerberos ticket is checked by name |
| Time | NTP sync (preferably with the domain controllers) | Kerberos and app-generated codes fail if the clocks differ by more than 5 minutes |
| Package access | A mirror or local repository of the distribution | The package installs PostgreSQL from it if it is missing |

The server does not need internet access.

### Active Directory domain

| What | Where to get it |
|---|---|
| Domain name (`corp.example.test`) and NetBIOS name (`CORP`) | Domain properties in "Active Directory Users and Computers" |
| Full names of the controllers (`dc1.corp.example.test`, `dc2.…`) | "Active Directory Sites and Services" or `nltest /dclist:CORP` |
| Controllers answer over LDAPS (TCP 636) | Usually yes if the domain has AD CS. Otherwise, see [section 15](#controllers-do-not-respond-on-ldaps) |
| Root CA certificate that signs the controllers (PEM) | If the CA is internal: export it from "Trusted Root Certification Authorities" in Base64 |
| Service account for reading the directory, for example `svc-pult@corp.example.test` | A regular domain user with no extra rights, a long password, and a password that does not expire |

The service account only reads the directory. The portal makes changes in AD (unlock, password reset, groups) with
a different account. You set that up later, and only if you need it: [directory.md](directory.md) (Russian).

### Network

| From | To | Port | Why |
|---|---|---|---|
| Employee PCs | Pult server | TCP 443 | Portal |
| Pult server | Domain controllers | TCP 636 (LDAPS) or 389 (StartTLS) | Sign-in and directory sync |
| Pult server | Mail server | SMTP port from the settings | Notifications (optional) |
| Pult server | PostgreSQL server | TCP 5432 | Only if the database is on another server |
| Agents on PCs | Pult server | TCP 443 | Remote support (optional) |
| Engineer PCs | Employee PCs | TCP 445 and dynamic RPC | Shadow connection: [admin-guide.md](admin-guide.md) (Russian) |

### Check the server with one command

The `install.sh` script from the release checks everything in the tables above that can be seen from the server,
and explains what to fix. It changes nothing. The script prints in English when `LANG` is not Russian.

```bash
sudo sh install.sh --check --dc dc1.corp.example.test --dc dc2.corp.example.test
```

## 3. Where to get the installer

On the **Releases** page of the repository, each version has these files:

| File | What it is |
|---|---|
| `pult_<version>_amd64.deb` | Server: installation and update |
| `install.sh` | Server check and package installation (optional) |
| `pult-agent.msi` | Remote support agent for employee PCs (Windows) |
| `pult-helper.exe` | Engineer helper for connecting to PCs (Windows) |
| `SHA256SUMS` | Checksums for all files |

Download the `.deb`, `install.sh` and `SHA256SUMS` to the server, into one directory, and check the checksums:

```bash
sha256sum -c --ignore-missing SHA256SUMS
```

Each line must end with `OK`. To build the package from source, see [CONTRIBUTING.md](../CONTRIBUTING.md), section
"Building the installers".

## 4. Installation

Script option: checks first, then installation. At the end, the script shows the address and the wizard code:

```bash
sudo sh install.sh ./pult_<version>_amd64.deb
```

Or directly, without checks:

```bash
sudo apt install ./pult_<version>_amd64.deb
```

The package:

- installs PostgreSQL if it is missing (from the distribution repository);
- creates the system user `pult`, the database, and the role `pult` with a random password;
- writes the settings to `/etc/pult/pult.env` (permissions `root:pult 0640`: the file holds the database password);
- creates the encryption key `/var/lib/pult/secret.key` and a temporary HTTPS certificate;
- applies the database migrations and starts the `pult` service on port 443.

Where things are:

| Path | What |
|---|---|
| `/opt/pult/bin/pult` | Program (server and all commands) |
| `/etc/pult/pult.env` | Service settings ([section 13](#13-server-parameters)); the package creates it once and does not change it after that |
| `/var/lib/pult/` | Data: attachments, portal certificate, `secret.key` |
| `/var/lib/pult/backups/` | Backups |
| `pult.service` | systemd service: `systemctl status pult`, log: `journalctl -u pult` |

Check: `systemctl is-active pult` → `active`; `curl -k https://localhost/health` →
`{"status":"ok",…,"database":"up"}`.

## 5. First-run wizard

Open `https://server-name/`. The browser warns about the temporary certificate. This is expected. The real
certificate is installed at the end ([section 6](#6-portal-certificate)). The wizard language is in the upper right
corner (Russian, English, 中文).

1. **Setup code.** It protects the wizard from outsiders until the portal is configured. The server writes the code
   to the log:

   ```bash
   sudo journalctl -u pult | grep "SETUP CODE" | tail -n 1
   ```

   The code is the word right after `SETUP CODE:`. A new code is generated each time the service starts. The
   latest code is the valid one.

2. **Domain.** Domain name, controllers (full DNS names, in order of priority), the service account and its
   password, and the root CA certificate if the controllers are signed by an internal CA. Connection mode: LDAPS
   (default) or StartTLS.
3. **"Test connection"** shows the state of each controller and what to fix. The domain settings are
   saved only after a controller accepts the administrator password in step 4.
4. **Administrator sign-in.** Sign in with your domain account. You become the super administrator. Employees are
   synced from AD automatically.
5. **Organization.** The wizard continues with: language and name, logo, modules, sites, ticket categories, mail.
   You can skip any step and return to it in "Settings → System".

What to configure next, and why: [guide.md](guide.md), section 2, "First steps after installation".

## 6. Portal certificate

The temporary certificate is good only for testing. Options:

- **Certificate from your own CA** (AD CS or any internal CA): a request for the portal name with the purpose
  "Server Authentication". PCs in the domain trust it automatically. If you have no CA, see
  [ad-setup.md](ad-setup.md) (Russian), section "Certificates".
- **Public certificate** (your own, or the organization's wildcard): if the portal is open to the internet.

Upload: **Settings → System → Portal certificate**: the certificate with its chain and the key, in PEM format. It
applies immediately, without a restart.

Certificate from files instead of the admin panel, in `/etc/pult/pult.env`:

```bash
PULT_TLS_CERT=/etc/pult/tls/pult.chain.crt
PULT_TLS_KEY=/etc/pult/tls/pult.key
# Second pair for another portal name (for example, a public one): chosen by the name the client asked for
PULT_TLS_EXTRA_CERT=/etc/pult/tls/public.chain.crt
PULT_TLS_EXTRA_KEY=/etc/pult/tls/public.key
```

Key files: `root:pult 0640`. After editing: `sudo systemctl restart pult`.

**HTTPS on a reverse proxy** (nginx, HAProxy, etc.):

```bash
PULT_AUTO_TLS=0                  # the portal listens on plain HTTP
PULT_LISTEN=127.0.0.1:8080       # address the proxy forwards requests to
PULT_TRUSTED_PROXIES=192.0.2.5   # proxy addresses: the portal trusts X-Forwarded-For only from them
```

Proxy addresses are required. Otherwise all users appear as one address: the sign-in attempt limit triggers for
everyone at once, and sign-ins from the internet cannot be told apart from sign-ins from the office.

## 7. Optional steps

| What | Why | Where |
|---|---|---|
| Kerberos (ticket check) | Groundwork for sign-in without a password: the "Status" page shows whether the portal recognizes the PC account. Ticket-based sign-in itself is not implemented in 0.1.0 | [ad-setup.md](ad-setup.md), sections 3–4 |
| Internet access | Sign-in from phones without VPN, with an optional second factor | [go-live.md](go-live.md) (Russian) |
| Remote support agent | List of sessions on PCs, engineer connection from the card | [admin-guide.md](admin-guide.md) |
| Changes to AD from the portal | Unlock, password reset, groups, access requests | [directory.md](directory.md) |
| Migration from an old phone book | Employees and phone numbers from SQL Server | `pult import phonebook --help` |

For the Kerberos check, set the service keytab in `/etc/pult/pult.env`:
`KRB5_KTNAME=/etc/pult/pult-http.keytab` (file `root:pult 0640`), then run `sudo systemctl restart pult`.

## 8. Backups and restore

Backups are made every night at 02:30 and before each update, in `/var/lib/pult/backups/pult-<date>/`. The last 14
are kept. A backup contains `db.dump` (the database), `files/` (attachments), and `tls/` (the portal certificate
**including its private key**, so treat backups as secret). The result of the last backup is shown in the admin panel.
Super administrators are notified if a backup fails.

Manual backup:

```bash
sudo -u pult sh -c 'set -a; . /etc/pult/pult.env; set +a; cd /var/lib/pult; /opt/pult/bin/pult backup'
```

**The encryption key `/var/lib/pult/secret.key` is not included in the backup.** Anyone who gets the backup cannot
decrypt the service account passwords (AD, mail, PBX). So right after installation, store the key separately from
the backups, for example on a removable medium in a safe or in a password manager:

```bash
sudo install -m 0600 /var/lib/pult/secret.key /media/<media>/pult-secret.key
```

Copy backups to another server or a NAS (rsync, a backup agent). A copy on the same disk does not help if the
server is lost.

Restore from the backup `/var/lib/pult/backups/pult-<date>`:

```bash
B=/var/lib/pult/backups/pult-<date>
sudo systemctl stop pult
sudo -u postgres dropdb pult && sudo -u postgres createdb -O pult pult
sudo cat "$B/db.dump" | sudo -u postgres pg_restore -d pult    # the backup directory is readable only by pult and root
for d in files tls; do
  sudo rm -rf "/var/lib/pult/$d"
  if sudo test -d "$B/$d"; then sudo cp -a "$B/$d" /var/lib/pult/ && sudo chown -R pult:pult "/var/lib/pult/$d"; fi
done
sudo install -o pult -g pult -m 0600 /media/<media>/pult-secret.key /var/lib/pult/secret.key
sudo systemctl start pult
```

On a new server, install the package of the same or a newer version, copy the backup directory to the server, and
run the same commands. If the package is newer than the backup, the service applies the migrations itself at start.

If the key is lost, the service creates a new one, and the saved passwords become unreadable. Set the AD service
account password again with `pult setup ad` (the same parameters as in the wizard; sign-in does not work without
it). Mail, PBX and webhook passwords are set again in the admin panel.

## 9. Update

A new version is installed the same way:

```bash
sudo sh install.sh ./pult_<new version>_amd64.deb
# or
sudo apt install ./pult_<new version>_amd64.deb
```

The package stops the service, makes a backup, applies the migrations in **one transaction**, and starts the new
version. If the backup fails or the migration fails, the database stays as it was, the previous program version is
brought back, and the service starts on it. `apt` reports the error; for details, see `journalctl -u pult`. To retry
the update after the cause is fixed: `sudo dpkg --configure -a`.

Before updating, read the section for your version in `CHANGELOG.md`. You can go back to the previous version by
hand only while the database has not been upgraded: `sudo apt install --allow-downgrades ./pult_<previous>_amd64.deb`.
After the migrations have run, restore the backup first ([section 8](#8-backups-and-restore)).

## 10. Removal

```bash
sudo apt remove pult     # the program and the service; data stays
sudo apt purge pult      # the same; data still stays, delete it by hand
```

The package never deletes data on its own. The `pult` database, `/etc/pult`, and `/var/lib/pult` (the key and the
backups) are needed to bring the portal back. Delete everything permanently only if the backups are no longer
needed:

```bash
sudo -u postgres dropdb pult && sudo -u postgres dropuser pult
sudo rm -rf /etc/pult /var/lib/pult
```

## 11. Docker instead of the package

For servers that already run everything in containers. Requires Docker with Compose:

```bash
git clone <repository URL> pult && cd pult
docker compose -f deploy/compose.yml up -d --build
```

The database password, encryption key and temporary certificate are created automatically. The portal runs on port
443 (for another port, set `PULT_HTTPS_PORT=8443` before the command). Wizard code:

```bash
docker compose -f deploy/compose.yml logs pult | grep "SETUP CODE" | tail -n 1
```

Update: `git pull && docker compose -f deploy/compose.yml up -d --build`. The migrations are the same, in one
transaction. Backup: the `pult_pgdata`, `pult_data` and `pult_secrets` volumes (the last one holds the database
password; without it the restored database cannot be opened). Docker makes no backup on update, so make one
before `up -d --build`. `pult_data` holds `secret.key`; keep a copy of it separately from the database backup.

When the portal is accessed from the internet, on some systems Docker replaces the client address with an address
from its own network (`172.x`). See [go-live.md](go-live.md), paragraph "Docker".

## 12. Database on another server

Before installing, install only the client instead of PostgreSQL: `sudo apt install postgresql-client`. On the
database server (PostgreSQL 15 or newer), the administrator creates the role and the database:

```sql
CREATE ROLE pult LOGIN PASSWORD '…';
CREATE DATABASE pult OWNER pult;
\c pult
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS unaccent;
```

After installing the package, put the address into `/etc/pult/pult.env` and restart the service:

```bash
PULT_DATABASE_URL=postgres://pult:PASSWORD@db.corp.example.test:5432/pult
```

```bash
sudo systemctl restart pult
```

The password can be kept in a separate file: leave the address without a password in `PULT_DATABASE_URL`
(`postgres://pult@db.corp.example.test:5432/pult`) and add `PULT_DATABASE_PASSWORD_FILE=/etc/pult/db-password`
(file `root:pult 0640`).

## 13. Server parameters

All parameters are variables in `/etc/pult/pult.env` (after editing, run `sudo systemctl restart pult`). They can
also be set in `/var/lib/pult/pult.toml` (another path: `PULT_CONFIG`). Environment variables take precedence
over the file; an unknown key in the file stops the service from starting. The example
`deploy/pult.example.toml` is in the repository, not in the package.

| Variable | Default value | What it sets |
|---|---|---|
| `PULT_DATABASE_URL` | set by the package | PostgreSQL database address |
| `PULT_DATABASE_PASSWORD_FILE` | — | Database password from a file; inserted into `PULT_DATABASE_URL`, which must be set |
| `PULT_LISTEN` | `0.0.0.0:443` (package) | Service address and port |
| `PULT_AUTO_TLS` | `1` (package) | Certificate from the admin panel; `0` means no HTTPS (behind a proxy) |
| `PULT_TLS_CERT`, `PULT_TLS_KEY` | — | Certificate and key from files |
| `PULT_TLS_EXTRA_CERT`, `PULT_TLS_EXTRA_KEY` | — | Second pair for another portal name |
| `PULT_TRUSTED_PROXIES` | — | Addresses or networks of reverse proxies, comma-separated |
| `PULT_DATA_DIR` | `/var/lib/pult` (package) | Attachments, certificate, key |
| `PULT_BACKUP_DIR` | `<PULT_DATA_DIR>/backups` | Backup directory |
| `PULT_SECRET_KEY` | file `secret.key` | Encryption key as a string (at least 32 characters) |
| `PULT_INITIAL_SUPERADMIN` | — | Login that becomes super administrator at first sign-in |
| `PULT_METRICS_TOKEN` | — | Token for scraping `/metrics` (Prometheus) from another host; without it `/metrics` answers only on localhost |
| `PULT_LOG_FORMAT` | `json` (package) | Log format: `json` or `text` |
| `KRB5_KTNAME` | — | Keytab for the Kerberos check |

A template with all the variables is `.env.example` in the root of the repository.

## 14. pult commands

Commands run with the service settings:

```bash
sudo -u pult sh -c 'set -a; . /etc/pult/pult.env; set +a; cd /var/lib/pult; /opt/pult/bin/pult <command>'
```

| Command | What it does |
|---|---|
| `pult version` | Version |
| `pult check db` | Database connection and migration state |
| `pult check ad` | Service account and directory sync. Reads the connection from the `[ad]` section of `pult.toml`, not from the database: a domain set up in the wizard needs that file (password: `PULT_AD_BIND_PASSWORD`) |
| `pult check ad --login ivanov` | Sign-in for this account (asks for the password without showing it) |
| `pult backup` | Back up now |
| `pult migrate` | Apply migrations (the service does this itself at start) |
| `pult setup ad …` | Set the domain connection without the wizard (`--help` for options) |
| `pult import phonebook …` | Migration from an old phone book on SQL Server (`--help`) |
| `pult serve` | Start the server (the service does this) |

## 15. Troubleshooting

| Symptom | Cause and what to do |
|---|---|
| `apt` cannot install `postgresql` | No access to the mirror: `sudo apt update`, connect a mirror or local repository; or use a database on another server ([section 12](#12-database-on-another-server)) |
| "database not configured — service NOT started" (база не настроена — служба НЕ запущена) | PostgreSQL was not found on the server: set `PULT_DATABASE_URL` and run `sudo systemctl restart pult` |
| The service does not start, port 443 is in use | Another program listens on 443 (`sudo ss -ltnp 'sport = :443'`): stop it or set `PULT_LISTEN` |
| The page does not open from a PC | Firewall on the server or the network: open TCP 443 from employee networks; check the DNS record |
| The wizard does not accept the code | The code changes each time the service starts. Use the latest one from the log |
| "The controller dropped the TLS connection" | The controller has no LDAPS certificate: see below |
| Controller certificate check error | Upload the root CA certificate (PEM) in the wizard; the controller name must match the SAN of the certificate |
| "Wrong login or password" with the correct password | The account is disabled, the password has expired, or a password change is required: check the account in AD and "Settings → AD directory → Test connection"; `pult check ad --login <login>` shows the exact cause (needs `pult.toml` with `[ad]`) |
| "Status" does not recognize the account via Kerberos | The portal is opened by IP address, not by name; no keytab or SPN; the site is not in the "Local intranet" zone: [ad-setup.md](ad-setup.md) |
| The update failed | Failed at the backup or migration: the database is unchanged and the previous version is running. The service did not start after the migrations: the database is already new and the previous version will not start with it; the cause is in `journalctl -u pult`, retry with `sudo dpkg --configure -a`, restore a backup if needed ([section 8](#8-backups-and-restore)) |

### Controllers do not respond on LDAPS

The portal connects to AD only over a secure connection. If the wizard shows "The controller dropped the TLS
connection", the controller has no certificate:

- **The domain has AD CS.** Controllers usually get a certificate automatically (template "Domain Controller" or
  "Kerberos Authentication"). Restarting the controller or running `certutil -pulse` speeds up issuance.
- **No AD CS.** Install it, or issue a certificate another way. Requirements: the certificate is in the computer
  store on the controller, the full controller name is in the SAN, and the purpose is "Server Authentication".
  Ready-made tools for the no-AD CS case are in [ad-setup.md](ad-setup.md).
- If only port 636 is blocked and the controller has a certificate, choose StartTLS (port 389) in the wizard.
  `sudo sh install.sh --check --dc <controller>` shows which option works.

The service log is the first place to look for the cause: `sudo journalctl -u pult -n 200`.
