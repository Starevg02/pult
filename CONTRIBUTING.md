# Как внести вклад в Pult

[English](#english)

## Русский

Спасибо, что хотите помочь. Уязвимости не обсуждаем в issue: см. [SECURITY.md](SECURITY.md).
Как устроен код — [docs/architecture.md](docs/architecture.md).

### Окружение разработчика

Нужно: Rust 1.88 (`rust-toolchain.toml` подтянет сам), Node.js 24+, Docker с Compose; для сборки на Linux —
`libkrb5-dev` и `libclang-dev` (проверка Kerberos).

```bash
git config core.hooksPath .githooks          # проверки безопасности перед коммитом (обязательно)
cp .env.example .env.local                   # заменить CHANGEME
docker compose -f deploy/compose.dev.yml up -d --build   # PostgreSQL, два контроллера Samba AD, почта для тестов
cat deploy/dev/ca/dc*-ca.pem > deploy/dev/ca/all-ca.pem
cp deploy/pult.example.toml pult.toml        # заменить CHANGEME

cargo run -p pult-server -- migrate          # миграции
cargo run -p pult-server -- serve            # http://127.0.0.1:8080/health, /metrics
```

Окружение слушает только `127.0.0.1`. Тестовые учётки Samba (`deploy/dev/samba/seed-users.sh`): `ivanov`,
`petrova` — рабочие; `disabled`, `mustchange`, `expired` — для проверки сообщений экрана входа.

Посмотреть портал без домена: `cargo run -p pult-server --features demo -- demo` (нужен `PULT_DATABASE_URL`
на пустую базу) — поддельный домен с вымышленными сотрудниками, вход `admin`, пароль печатается при запуске.
Демо-сборку нельзя упаковать в `.deb`: `build.sh` её отклонит.

Веб-интерфейс (`web/`: React, TypeScript, Vite, TanStack Router/Query, Tailwind, i18next):

```bash
cd web && npm ci                             # без скриптов установки пакетов (web/.npmrc)
npm run dev                                  # http://localhost:5173, API — через прокси на :8080
npm run build                                # web/dist → вшивается в release-сборку сервера
# После изменения API сервера — обновить типизированный клиент:
mkdir -p ../target && cargo run -q -p pult-server -- openapi > ../target/openapi.json && npm run gen:api
```

Проверки подключений: `cargo run -p pult-server -- check db`, `… check ad`, `… check ad --login ivanov`.

### Хуки (обязательно)

`.githooks/pre-commit` не даёт закоммитить ключи и пароли, незащищённый LDAP, отключённую проверку TLS и
SQL, собранный через `format!`. Ложное срабатывание помечается комментарием `// SAFE: <причина>` в той же
строке. Проверить всё дерево: `.githooks/pre-commit --all`. Свои запрещённые строки (имена серверов и
доменов своей организации) — по одной в `.git/info/forbidden`: хук не пропустит их в коммит.

### Перед pull request

Rust:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Тестам с базой нужен PostgreSQL и переменная `PULT_TEST_DATABASE_URL`. Без неё локально они пропускаются,
в CI отсутствие переменной — ошибка. CI также запускает `cargo deny` и `npm audit`.

Веб-интерфейс:

```bash
cd web && npm ci && npm run typecheck && npm run lint && npm run build
```

### Сборка установщиков

Сервер (`.deb`), на Debian 13 или Ubuntu 24.04 — нужны ещё `dpkg-dev` и `binutils`:

```bash
(cd web && npm ci && npm run build)
cargo build --release --locked -p pult-server
sh deploy/deb/build.sh target/release/pult target/deb    # → target/deb/pult_<версия>+<время>.<коммит>_amd64.deb
```

Агент и helper для Windows (на Windows, нужен .NET SDK для WiX 5):

```powershell
cargo build --release --locked -p pult-agent -p pult-helper
dotnet tool install wix --version 5.0.2 --tool-path target/wix
./target/wix/wix extension add -g WixToolset.Util.wixext/5.0.2
./target/wix/wix build -arch x64 -ext WixToolset.Util.wixext -d Version=0.1.0 `
  -d AgentExe=target/release/pult-agent.exe deploy/windows/agent.wxs -o target/release/pult-agent.msi
```

Выпуск: версия в `Cargo.toml` и `web/package.json`, раздел в `CHANGELOG.md`, затем тег `vX.Y.Z`.
`.github/workflows/release.yml` соберёт пакет, MSI, helper, `install.sh` и `SHA256SUMS` в черновик релиза;
мейнтейнер проверяет и публикует его.

### Коммиты и PR

- Заголовок коммита — Conventional Commits: `тип(область): кратко`. Типы: `feat`, `fix`, `docs`, `security`,
  `test`, `chore`. Можно по-русски или по-английски.
- Один PR — одна тема. В описании: что изменили, зачем и как проверили.
- Миграции в `migrations/` только вперёд: новый файл, а не правка старого.
- Заметное пользователю изменение — строка в `CHANGELOG.md`.

### Переводы

- Русский — исходный язык. Все строки интерфейса берутся по ключам `t("…")` из `web/src/i18n/ru.json`,
  `en.json` и `zh.json`. Линтер не пропустит русский текст прямо в разметке.
- Тексты ошибок, подписи отчётов и письма на сервере — в `crates/pult-core/src/l10n/en.tsv` и `zh.tsv`. Тест
  `crates/pult-core/tests/l10n_coverage.rs` падает, если русская строка не переведена или в каталоге осталась
  лишняя запись.
- Новая строка — перевод на все три языка в том же PR. Подробности: [docs/localization.md](docs/localization.md).

### Данные организаций

В коде, тестах, документации и примерах не должно быть реальных имён серверов, доменов, IP-адресов,
учётных записей, имён людей, ключей и паролей. В примерах используйте `example.com`, `.test` и адреса из
документационных диапазонов (например, `192.0.2.0/24`). Секреты — только в шаблонах с `CHANGEME`, а
локальные `.env.local` и `pult.toml` остаются в `.gitignore`.

### Лицензия вклада

Отправляя изменения, вы соглашаетесь, что они распространяются по лицензии MIT ([LICENSE](LICENSE)).

### Issue

Ошибки — шаблон «Ошибка», предложения — шаблон «Предложение». Вопросы по установке сначала смотрите в
[docs/install.md](docs/install.md).

## English

Thank you for helping. Please do not discuss vulnerabilities in issues: see [SECURITY.md](SECURITY.md).
Code overview (Russian): [docs/architecture.md](docs/architecture.md).

### Development environment

You need Rust 1.88 (installed from `rust-toolchain.toml`), Node.js 24+, Docker with Compose; on Linux also
`libkrb5-dev` and `libclang-dev` (Kerberos ticket check).

```bash
git config core.hooksPath .githooks          # security checks before each commit (required)
cp .env.example .env.local                   # replace CHANGEME
docker compose -f deploy/compose.dev.yml up -d --build   # PostgreSQL, two Samba AD controllers, test mail
cat deploy/dev/ca/dc*-ca.pem > deploy/dev/ca/all-ca.pem
cp deploy/pult.example.toml pult.toml        # replace CHANGEME
cargo run -p pult-server -- migrate
cargo run -p pult-server -- serve            # http://127.0.0.1:8080/health
```

The environment listens on `127.0.0.1` only. Samba test accounts: `ivanov`, `petrova` (working); `disabled`,
`mustchange`, `expired` (sign-in error messages). Demo without a domain:
`cargo run -p pult-server --features demo -- demo` with `PULT_DATABASE_URL` pointing to an empty database;
the `admin` password is printed at start.

Web UI: `cd web && npm ci && npm run dev` (http://localhost:5173, API proxied to :8080). After a server API
change, regenerate the client:
`mkdir -p ../target && cargo run -q -p pult-server -- openapi > ../target/openapi.json && npm run gen:api`.

### Git hooks (required)

`.githooks/pre-commit` blocks keys and passwords, plain LDAP, disabled TLS verification and SQL built with
`format!`. Mark a false positive with a `// SAFE: <reason>` comment on the same line. Check the whole tree:
`.githooks/pre-commit --all`. Add your organization's server and domain names, one per line, to
`.git/info/forbidden` and the hook will keep them out of commits.

### Before opening a pull request

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd web && npm ci && npm run typecheck && npm run lint && npm run build
```

Database tests need PostgreSQL and `PULT_TEST_DATABASE_URL`. Locally they are skipped without it; in CI a
missing variable is an error. CI also runs `cargo deny` and `npm audit`.

### Building the installers

Server package on Debian 13 or Ubuntu 24.04 (also needs `dpkg-dev` and `binutils`):

```bash
(cd web && npm ci && npm run build)
cargo build --release --locked -p pult-server
sh deploy/deb/build.sh target/release/pult target/deb
```

Windows agent MSI and helper: see the PowerShell block in the Russian section above (WiX 5, .NET SDK).

Release: bump the version in `Cargo.toml` and `web/package.json`, add a `CHANGELOG.md` section, push a
`vX.Y.Z` tag. `.github/workflows/release.yml` builds a draft release with the package, MSI, helper,
`install.sh` and `SHA256SUMS`; a maintainer reviews and publishes it.

### Commits and pull requests

- Commit subject: Conventional Commits, `type(scope): summary`. Types: `feat`, `fix`, `docs`, `security`,
  `test`, `chore`. Russian or English.
- One topic per PR. Describe what changed, why, and how you checked it.
- Migrations in `migrations/` are forward-only: add a new file, do not edit an old one.
- A user-visible change gets an entry in `CHANGELOG.md`.

### Translations

- Russian is the source language. All interface strings use `t("…")` keys in `web/src/i18n/ru.json`,
  `en.json` and `zh.json`. The linter rejects Russian text placed directly in markup.
- Server error messages, report labels and e-mails live in `crates/pult-core/src/l10n/en.tsv` and `zh.tsv`.
  The test `crates/pult-core/tests/l10n_coverage.rs` fails on an untranslated Russian string or a stale entry.
- A new string comes with translations for all three languages in the same PR.

### Organization data

Code, tests, docs and examples must not contain real server or domain names, IP addresses, account names,
people's names, keys or passwords. Use `example.com`, `.test`, and documentation address ranges (for example
`192.0.2.0/24`). Secrets belong only in `CHANGEME` templates; local `.env.local` and `pult.toml` stay in
`.gitignore`.

### License of contributions

By submitting changes you agree that they are distributed under the MIT License ([LICENSE](LICENSE)).

### Issues

Use the bug report template for bugs and the feature request template for ideas. For installation questions,
check [docs/install.en.md](docs/install.en.md) first.
