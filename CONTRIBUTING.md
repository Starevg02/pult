# Как внести вклад в Pult · Contributing · 参与贡献

[Русский](#русский) | [English](#english) | [中文](#中文)

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
./target/wix/wix build -arch x64 -ext WixToolset.Util.wixext -d Version=0.2.0 `
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
How the code is organized: [docs/architecture.md](docs/architecture.md) (in Russian).

### Development environment

You need Rust 1.88 (installed from `rust-toolchain.toml`), Node.js 24+, Docker with Compose; to build on Linux
also `libkrb5-dev` and `libclang-dev` (Kerberos ticket check).

```bash
git config core.hooksPath .githooks          # security checks before each commit (required)
cp .env.example .env.local                   # replace CHANGEME
docker compose -f deploy/compose.dev.yml up -d --build   # PostgreSQL, two Samba AD controllers, test mail
cat deploy/dev/ca/dc*-ca.pem > deploy/dev/ca/all-ca.pem
cp deploy/pult.example.toml pult.toml        # replace CHANGEME

cargo run -p pult-server -- migrate          # migrations
cargo run -p pult-server -- serve            # http://127.0.0.1:8080/health, /metrics
```

The environment listens on `127.0.0.1` only. Samba test accounts (`deploy/dev/samba/seed-users.sh`): `ivanov`,
`petrova` are working accounts; `disabled`, `mustchange`, `expired` test the sign-in screen messages.

To see the portal without a domain: `cargo run -p pult-server --features demo -- demo` (needs
`PULT_DATABASE_URL` pointing to an empty database). It is a fake domain with made-up employees; sign in as
`admin`, the password is printed at start. A demo build cannot be packaged as `.deb`: `build.sh` rejects it.

Web interface (`web/`: React, TypeScript, Vite, TanStack Router/Query, Tailwind, i18next):

```bash
cd web && npm ci                             # without package install scripts (web/.npmrc)
npm run dev                                  # http://localhost:5173, API proxied to :8080
npm run build                                # web/dist is embedded into the release build of the server
# After a server API change, regenerate the typed client:
mkdir -p ../target && cargo run -q -p pult-server -- openapi > ../target/openapi.json && npm run gen:api
```

Connection checks: `cargo run -p pult-server -- check db`, `… check ad`, `… check ad --login ivanov`.

### Git hooks (required)

`.githooks/pre-commit` blocks keys and passwords, plain LDAP, disabled TLS verification and SQL built with
`format!`. Mark a false positive with a `// SAFE: <reason>` comment on the same line. Check the whole tree:
`.githooks/pre-commit --all`. Add your organization's own forbidden strings (server and domain names), one
per line, to `.git/info/forbidden` and the hook will keep them out of commits.

### Before opening a pull request

Rust:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Database tests need PostgreSQL and the `PULT_TEST_DATABASE_URL` variable. Locally they are skipped without
it; in CI a missing variable is an error. CI also runs `cargo deny` and `npm audit`.

Web interface:

```bash
cd web && npm ci && npm run typecheck && npm run lint && npm run build
```

### Building the installers

Server package (`.deb`) on Debian 13 or Ubuntu 24.04; also needs `dpkg-dev` and `binutils`:

```bash
(cd web && npm ci && npm run build)
cargo build --release --locked -p pult-server
sh deploy/deb/build.sh target/release/pult target/deb    # → target/deb/pult_<version>+<time>.<commit>_amd64.deb
```

Windows agent and helper (on Windows, needs the .NET SDK for WiX 5):

```powershell
cargo build --release --locked -p pult-agent -p pult-helper
dotnet tool install wix --version 5.0.2 --tool-path target/wix
./target/wix/wix extension add -g WixToolset.Util.wixext/5.0.2
./target/wix/wix build -arch x64 -ext WixToolset.Util.wixext -d Version=0.2.0 `
  -d AgentExe=target/release/pult-agent.exe deploy/windows/agent.wxs -o target/release/pult-agent.msi
```

Release: the version in `Cargo.toml` and `web/package.json`, a section in `CHANGELOG.md`, then a `vX.Y.Z`
tag. `.github/workflows/release.yml` builds the package, MSI, helper, `install.sh` and `SHA256SUMS` into a
draft release; a maintainer reviews and publishes it.

### Commits and pull requests

- Commit subject: Conventional Commits, `type(scope): summary`. Types: `feat`, `fix`, `docs`, `security`,
  `test`, `chore`. Russian or English.
- One topic per pull request. In the description: what changed, why, and how it was checked.
- Migrations in `migrations/` are forward-only: add a new file, do not edit an old one.
- A user-visible change gets an entry in `CHANGELOG.md`.

### Translations

- Russian is the source language. All interface strings are taken by `t("…")` keys from
  `web/src/i18n/ru.json`, `en.json` and `zh.json`. The linter rejects Russian text placed directly in markup.
- Server error messages, report labels and e-mails are in `crates/pult-core/src/l10n/en.tsv` and `zh.tsv`.
  The test `crates/pult-core/tests/l10n_coverage.rs` fails if a Russian string is not translated or the
  catalog has a stale entry.
- A new string comes with translations into all three languages in the same pull request. Details:
  [docs/localization.md](docs/localization.md) (in Russian).

### Organization data

Code, tests, documentation and examples must not contain real server or domain names, IP addresses,
accounts, people's names, keys or passwords. In examples use `example.com`, `.test` and documentation
address ranges (for example `192.0.2.0/24`). Secrets belong only in templates with `CHANGEME`; local
`.env.local` and `pult.toml` stay in `.gitignore`.

### License of contributions

By submitting changes you agree that they are distributed under the MIT License ([LICENSE](LICENSE)).

### Issues

Use the bug report template for bugs and the feature request template for ideas. For installation questions,
check [docs/install.en.md](docs/install.en.md) first.

## 中文

感谢您的帮助。请不要在 issue 中讨论漏洞，参见 [SECURITY.md](SECURITY.md)。
代码结构说明：[docs/architecture.md](docs/architecture.md)（俄文）。

### 开发环境

需要 Rust 1.88（由 `rust-toolchain.toml` 自动安装）、Node.js 24+、带 Compose 的 Docker；在 Linux 上构建
还需要 `libkrb5-dev` 和 `libclang-dev`（Kerberos 票据检查）。

```bash
git config core.hooksPath .githooks          # 提交前的安全检查（必需）
cp .env.example .env.local                   # 替换 CHANGEME
docker compose -f deploy/compose.dev.yml up -d --build   # PostgreSQL、两个 Samba AD 域控制器、测试邮件
cat deploy/dev/ca/dc*-ca.pem > deploy/dev/ca/all-ca.pem
cp deploy/pult.example.toml pult.toml        # 替换 CHANGEME

cargo run -p pult-server -- migrate          # 数据库迁移
cargo run -p pult-server -- serve            # http://127.0.0.1:8080/health, /metrics
```

开发环境只监听 `127.0.0.1`。Samba 测试账户（`deploy/dev/samba/seed-users.sh`）：`ivanov`、`petrova` 为
正常账户；`disabled`、`mustchange`、`expired` 用于检查登录页面的提示信息。

无需域即可查看门户：`cargo run -p pult-server --features demo -- demo`（需要 `PULT_DATABASE_URL` 指向一个
空数据库）。这是一个带虚构员工的模拟域；以 `admin` 登录，密码在启动时打印。演示构建不能打包为 `.deb`：
`build.sh` 会拒绝它。

Web 界面（`web/`：React、TypeScript、Vite、TanStack Router/Query、Tailwind、i18next）：

```bash
cd web && npm ci                             # 不运行软件包安装脚本（web/.npmrc）
npm run dev                                  # http://localhost:5173，API 代理到 :8080
npm run build                                # web/dist 会嵌入服务器的 release 构建
# 修改服务器 API 后，重新生成类型化客户端：
mkdir -p ../target && cargo run -q -p pult-server -- openapi > ../target/openapi.json && npm run gen:api
```

连接检查：`cargo run -p pult-server -- check db`、`… check ad`、`… check ad --login ivanov`。

### Git 钩子（必需）

`.githooks/pre-commit` 会阻止提交密钥和密码、未加密的 LDAP、关闭的 TLS 校验以及用 `format!` 拼接的 SQL。
误报可在同一行用 `// SAFE: <原因>` 注释标记。检查整个代码树：`.githooks/pre-commit --all`。可将本组织
自己的禁用字符串（服务器名和域名）逐行写入 `.git/info/forbidden`，钩子会阻止它们进入提交。

### 提交 pull request 之前

Rust：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

涉及数据库的测试需要 PostgreSQL 和 `PULT_TEST_DATABASE_URL` 变量。本地没有该变量时会跳过这些测试；在 CI
中缺少该变量会报错。CI 还会运行 `cargo deny` 和 `npm audit`。

Web 界面：

```bash
cd web && npm ci && npm run typecheck && npm run lint && npm run build
```

### 构建安装包

服务器软件包（`.deb`），在 Debian 13 或 Ubuntu 24.04 上构建；还需要 `dpkg-dev` 和 `binutils`：

```bash
(cd web && npm ci && npm run build)
cargo build --release --locked -p pult-server
sh deploy/deb/build.sh target/release/pult target/deb    # → target/deb/pult_<版本>+<时间>.<提交>_amd64.deb
```

Windows 代理程序和辅助程序（在 Windows 上构建，WiX 5 需要 .NET SDK）：

```powershell
cargo build --release --locked -p pult-agent -p pult-helper
dotnet tool install wix --version 5.0.2 --tool-path target/wix
./target/wix/wix extension add -g WixToolset.Util.wixext/5.0.2
./target/wix/wix build -arch x64 -ext WixToolset.Util.wixext -d Version=0.2.0 `
  -d AgentExe=target/release/pult-agent.exe deploy/windows/agent.wxs -o target/release/pult-agent.msi
```

发布：更新 `Cargo.toml` 和 `web/package.json` 中的版本，在 `CHANGELOG.md` 中添加一节，然后打 `vX.Y.Z`
标签。`.github/workflows/release.yml` 会把软件包、MSI、辅助程序、`install.sh` 和 `SHA256SUMS` 构建到一个
草稿发布中；维护者检查后将其发布。

### 提交与 pull request

- 提交标题遵循 Conventional Commits：`type(scope): 简述`。类型：`feat`、`fix`、`docs`、`security`、
  `test`、`chore`。可用俄语或英语。
- 一个 pull request 只处理一个主题。描述中说明改了什么、为什么改、如何验证。
- `migrations/` 中的迁移只能向前：新建文件，不要修改旧文件。
- 用户可见的变更需在 `CHANGELOG.md` 中添加一条记录。

### 翻译

- 俄语是源语言。所有界面字符串都通过 `t("…")` 键取自 `web/src/i18n/ru.json`、`en.json` 和 `zh.json`。
  直接写在标记中的俄语文本会被 linter 拒绝。
- 服务器的错误信息、报表标签和邮件位于 `crates/pult-core/src/l10n/en.tsv` 和 `zh.tsv`。如果某条俄语字符串
  未翻译或目录中有多余条目，测试 `crates/pult-core/tests/l10n_coverage.rs` 会失败。
- 新字符串需在同一个 pull request 中提供三种语言的翻译。详见 [docs/localization.md](docs/localization.md)
  （俄文）。

### 组织数据

代码、测试、文档和示例中不得包含真实的服务器名、域名、IP 地址、账户、人名、密钥或密码。示例中请使用
`example.com`、`.test` 以及文档专用地址段（例如 `192.0.2.0/24`）。机密信息只能出现在带 `CHANGEME` 的
模板中；本地的 `.env.local` 和 `pult.toml` 保留在 `.gitignore` 中。

### 贡献的许可

提交更改即表示您同意这些更改按 MIT 许可证分发（[LICENSE](LICENSE)）。

### Issue

错误请使用"错误报告"模板，建议请使用"功能建议"模板。安装问题请先查阅
[docs/install.en.md](docs/install.en.md)（英文）。
