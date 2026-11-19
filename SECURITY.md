# Политика безопасности · Security policy · 安全策略

[Русский](#русский) | [English](#english) | [中文](#中文)

## Русский

### Поддерживаемые версии

| Версия | Исправления безопасности |
|---|---|
| 0.1.x (последняя) | Да |
| Более ранние сборки | Нет, обновитесь до последней (`CHANGELOG.md`) |

### Как сообщить об уязвимости

Не создавайте публичный issue и не обсуждайте уязвимость в комментариях, pull request'ах или чатах.

1. Откройте вкладку **Security** репозитория и выберите **Report a vulnerability**. Это приватный канал GitHub: отчёт видят только мейнтейнеры и вы.
2. Публичного адреса электронной почты для отчётов пока нет. Используйте только этот канал.

### Что указать в отчёте

- Версия Pult (например, 0.1.0) и способ установки: `.deb` или Docker Compose.
- Описание проблемы и её влияние: что может сделать злоумышленник и какие данные затронуты.
- Роль, с которой проверяется проблема: без входа, сотрудник, инженер или администратор.
- Шаги воспроизведения. Минимальный пример лучше длинного описания.
- Журналы и скриншоты, если они помогают. Перед отправкой удалите пароли, ключи, токены, адреса, имена и данные сотрудников.

Не присылайте реальные учётные данные и персональные данные. Замените их вымышленными значениями.

### Чего ожидать

Мы стараемся подтвердить получение отчёта и разобрать его, когда позволяет время. Мы не можем обещать конкретных сроков ответа или исправления. По возможности мы держим связь в приватном отчёте, сообщаем о решении и согласуем публикацию описания уязвимости после исправления. Если вы хотите, чтобы вас упомянули, напишите об этом в отчёте.

### Область действия

- Pult ставится на серверы организаций, и каждая организация администрирует свою установку. Проблемы в инфраструктуре вокруг Pult (сеть, роутер, DNS, контроллеры домена, PostgreSQL, обратный прокси) сами по себе вне области, но если Pult по умолчанию небезопасен или вводит администратора в заблуждение, сообщите и об этом.
- Рекомендации по запуску: `docs/install.md`, при доступе из интернета — `docs/go-live.md`. Модель защиты и известные ограничения — `docs/security.md`.
- Не проверяйте чужие установки и не нарушайте работу сервисов, которые вам не принадлежат.

## English

### Supported versions

| Version | Security fixes |
|---|---|
| 0.1.x (latest) | Yes |
| Earlier builds | No. Upgrade to the latest (see `CHANGELOG.md`) |

### How to report a vulnerability

Do not open a public issue and do not discuss the vulnerability in comments, pull requests or chats.

1. Open the **Security** tab of the repository and choose **Report a vulnerability**. This is GitHub's private vulnerability reporting: only the maintainers and you can see the report.
2. There is no public security e-mail address yet. Use this channel only.

### What to include

- The Pult version (for example, 0.1.0) and the installation method: `.deb` package or Docker Compose.
- A description of the problem and its impact: what an attacker could do and which data is affected.
- The role used for the test: not signed in, employee, engineer or administrator.
- Steps to reproduce. A minimal example is better than a long description.
- Logs or screenshots if they help. Before sending, remove passwords, keys, tokens, addresses, names and employee data.

Do not send real credentials or personal data. Replace them with made-up values.

### What to expect

We try to confirm receipt and review the report when time allows. We cannot promise specific response or fix times. Where possible we keep the discussion in the private report, tell you the decision, and agree on the disclosure of the advisory after a fix. If you want to be credited, say so in the report.

### Scope

- Pult is installed on organizations' own servers, and each organization administers its installation. Problems in the infrastructure around Pult (network, router, DNS, domain controllers, PostgreSQL, reverse proxy) are out of scope by themselves. If Pult is insecure by default or misleads the administrator, report that too.
- Deployment guidance: `docs/install.en.md` (installation), `docs/go-live.md` (access from the internet, Russian). Security model and known limitations: `docs/security.md` (Russian).
- Do not test installations you do not own and do not disrupt services that are not yours.

## 中文

### 受支持的版本

| 版本 | 安全修复 |
|---|---|
| 0.1.x（最新） | 是 |
| 更早的构建 | 否，请升级到最新版本（见 `CHANGELOG.md`） |

### 如何报告漏洞

请不要创建公开 issue，也不要在评论、pull request 或聊天中讨论漏洞。

1. 打开仓库的 **Security** 标签页，选择 **Report a vulnerability**。这是 GitHub 的私密漏洞报告渠道：只有维护者和您能看到报告。
2. 目前没有公开的安全邮箱，请只使用此渠道。

### 报告中应包含的内容

- Pult 版本（例如 0.1.0）和安装方式：`.deb` 软件包或 Docker Compose。
- 问题描述及其影响：攻击者可以做什么，涉及哪些数据。
- 复现问题时使用的角色：未登录、员工、工程师或管理员。
- 复现步骤。最小示例胜过冗长的描述。
- 有帮助的日志或截图。发送前请删除密码、密钥、令牌、地址、姓名和员工数据。

请不要发送真实的账户凭据或个人数据，请用虚构的值代替。

### 处理方式

我们会在时间允许时尽量确认收到报告并进行分析，但无法承诺具体的回复或修复时间。我们尽可能在私密报告中保持沟通，告知处理结论，并在修复后商定漏洞说明的公开时间。如果您希望被致谢，请在报告中注明。

### 范围

- Pult 安装在各组织自己的服务器上，每个组织自行管理其安装。Pult 周边基础设施（网络、路由器、DNS、域控制器、PostgreSQL、反向代理）本身的问题不在范围内；但如果 Pult 的默认配置不安全或会误导管理员，也请报告。
- 部署建议：`docs/install.en.md`（安装，英文），`docs/go-live.md`（互联网访问，俄文）。防护模型和已知限制：`docs/security.md`（俄文）。
- 请不要测试不属于您的安装，也不要干扰不属于您的服务。
