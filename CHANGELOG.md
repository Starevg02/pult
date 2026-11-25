# Изменения · Changelog · 变更记录

Формат — [Keep a Changelog](https://keepachangelog.com/ru/1.1.0/), версии — [SemVer](https://semver.org/lang/ru/).
До версии 1.0 возможны несовместимые изменения настроек и API; миграции базы всегда применяются автоматически.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions: [SemVer](https://semver.org/).
Before version 1.0, settings and the API may change incompatibly; database migrations are always applied
automatically.

格式：[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号：[SemVer](https://semver.org/lang/zh-CN/)。
1.0 版本之前，设置和 API 可能出现不兼容的变更；数据库迁移始终自动执行。

## 0.2.0 — оборудование, проблемы и изменения, хранилище секретов, помощник, мониторинг, вход через домен · hardware, problems and changes, secrets vault, assistant, monitoring, domain sign-in · 设备、问题与变更、密钥库、助手、监控、域登录

### Русский

**Оборудование**

- Выдача, возврат на склад и передача устройства с историей в карточке; «Моё оборудование» для сотрудника.
- Импорт каталога из CSV (`;` или `,`, UTF-8): проверка без записи с ошибками по строкам, затем импорт;
  заголовки — на русском, английском или китайском, доп. поля типа — по ключу или названию.
- «Найденные ПК»: ПК с агентом, которых нет в каталоге, принимаются в каталог после проверки инженером
  (ОС, IP, модель и серийный номер — из сведений агента) или отклоняются.
- Агент присылает оборудование и программы ПК (SMBIOS, процессор, память, диски, «Программы и компоненты»)
  при запуске и раз в сутки; в карточке ПК — эти сведения, в «Компьютеры» — поиск «Где установлена программа».
- CMDB: типы «Сервер» и «Сервис / ИС», связи между объектами (виды связей задаёт администратор),
  «Что затронет отказ» по связям с влиянием, карта зависимостей, предложения связей с подтверждением
  инженером, жизненный цикл по типам (состояния и разрешённые переходы).
- Лицензии ПО: купленные места и срок; установки считаются по программам из отчётов агента, лишние
  установки и истекающие лицензии отмечаются.
- Импорт из NetBox: устройства и виртуальные машины по API-токену раз в сутки; в каталог — после проверки
  инженером, принятые карточки сами не меняются.

**Проблемы и изменения**

- Новые типы заявок для инженеров: «Проблемы» (причина, обходное решение, известная ошибка, связанные
  инциденты, решение всех инцидентов одним ответом) и «Изменения» (риск, план, откат, окно работ).
- Согласование изменений по правилам «категория + уровень риска → согласующий»: решение в «Согласованиях»,
  отказ возвращает на доработку, правка начинает согласование заново, выполнить можно только согласованное.

**Хранилище секретов**

- Раздел «Секреты» для паролей служебных учёток и ключей: значение хранится зашифрованным, доступ задаётся
  у каждого секрета сотрудникам или ролям портала (карточка, значение, управление), а не группами AD.
- Показ значения — отдельное право, по кнопке и с причиной; каждый показ пишется в журнал аудита, само
  значение в журнал не попадает.

**Помощник**

- Помощник на своей модели за OpenAI-совместимым API (vLLM, Ollama и другие на отдельном сервере):
  ответы по базе знаний со ссылками на статьи, подсказка категории новой заявки, для инженеров — черновик
  ответа заявителю и краткое содержание заявки.
- Модели уходит только то, что пользователь и так видит; беседа не сохраняется; помощник только советует
  и ничего не делает сам. Включает и настраивает суперадминистратор, ключ API хранится зашифрованным.

**Мониторинг**

- Приём оповещений Zabbix, Prometheus Alertmanager и других систем по вебхуку с токеном источника:
  уведомление суперадминистраторам и, по порогу важности, заявка в канале «Мониторинг»; повтор не создаёт
  новую заявку, восстановление закрывает оповещение.
- Оповещения привязываются к объектам CMDB по узлу (название, IP, ПК с агентом); «Состояние сервисов»
  на главной инженера учитывает и аварии того, от чего сервис зависит.

**Вход и уведомления**

- Вход через домен без пароля (Kerberos) кнопкой «Войти через домен»: включается в «Безопасность», работает
  только из сетей организации, сеанс помечается методом входа.
- Вход через провайдера OpenID Connect (Keycloak, ADFS, Entra ID): код авторизации с PKCE, проверка подписи
  ID-токена (RS256, ES256), учётка из зеркала AD по логину, UPN или подтверждённой почте. Секрет клиента
  хранится зашифрованным. Из интернета вход включается отдельно и закрыт для тех, кому нужен код из приложения.
- Уведомления о заявках в Mattermost, Rocket.Chat, Slack (входящий вебхук) и Telegram (бот): событие, номер,
  тема, статус и ссылка, без текстов комментариев.

**Приложение на телефоне и уведомления на устройство**

- Портал устанавливается как приложение (PWA) на телефон и компьютер: значок, отдельное окно.
- Уведомления на устройство (Web Push), даже когда вкладка закрыта. Включает администратор («Система»),
  подписывает своё устройство сотрудник в профиле. Сообщения зашифрованы ключом браузера (RFC 8291) и подписаны
  ключом портала (VAPID); адреса подписок — только служб push браузеров. По умолчанию без темы заявки.
- Нативных приложений из магазинов нет: их заменяет PWA.

**Пароль домена**

- Смена своего пароля AD в «Моём профиле» (включается в настройках безопасности, нужен LDAPS). Портал входит
  под сотрудником со старым паролем, политику паролей проверяет домен. Неверный старый пароль считается
  неудачной попыткой входа; после смены закрываются сеансы на других устройствах.
- Решение о входе и синхронизации через Entra ID и Google Workspace и о нескольких доменах — ADR-0006.

**Резервные копии и масштаб**

- Сколько копий хранить (1–365) и проверять ли каждую — в «Системе» → «Резервная копия». Проверка читает дамп
  `pg_restore --list`, испорченная копия не сохраняется, суперадминистраторы получают уведомление.
- `pult backup` учитывает эти настройки и записывает результат, как ночная задача.
- Новый документ `docs/scale.md`: оценка оборудования, проверка нагрузки, несколько копий сервера, кластер
  PostgreSQL, установка без Интернета. Работа на 100 000+ сотрудников пока не подтверждена испытаниями.

**Расширения**

- Расширения — внешние сервисы по манифесту (ADR-0005): установка из файла, по адресу или из каталога
  организации. Портал показывает запрошенные права, выдаёт токен API только на чтение и подписанный вебхук,
  добавляет ссылки в меню по правам. Вебхук с событиями заявок — только у расширения, которое просит право
  `helpdesk.ticket.work`. Выключение отзывает токен; всё записывается в аудит.

**Коммуникации**

- Новый модуль «Коммуникации» (выключен, пока его не включит суперадминистратор): личные диалоги и групповые
  чаты. Читают только участники, текст хранится зашифрованным, удалять сообщения нельзя, правка — 15 минут с
  сохранением версий. Состав ведут владелец и администраторы чата, бывшие участники читают историю до выхода.
  Учётки, отключённые в AD, выбывают из групповых чатов, владение переходит администратору чата. Суперадминистратор
  видит групповые чаты без сообщений, назначает владельца и смотрит обезличенную статистику.

**Удалённая поддержка**

- Агент для Linux (systemd): кто вошёл (локально, SSH, XRDP), ОС, адреса и инвентаризация, включая пакеты dpkg
  или rpm. Подключения к сеансу на Linux нет — портал так и отвечает. Установка — руководство администратора,
  раздел 10.
- Helper: `register --run-as ДОМЕН\имя` — «Управление компьютером» и PowerShell запускаются через `runas /netonly`
  под админской учёткой инженера (аналог NetOnly), теневое подключение спрашивает её учётные данные.

**Самообслуживание**

- Каталог услуг: категории заявок карточками по разделам, с описанием и поиском; карточка открывает форму заявки
  с этой категорией и её полями. Описание категории задаётся в настройках заявок.
- Подробный опрос после решения: к оценке «хорошо / плохо» — необязательные баллы 1–5 за скорость, качество
  решения и вежливость. Средние баллы и число ответов — в отчётах и выгрузке.

**Публикация в Интернет**

- `pult edge nginx` выдаёт готовую конфигурацию пограничного прокси nginx для узла в DMZ: TLS, ограничение
  частоты входа и запросов, закрытые снаружи API агентов и helper, метрики и Kerberos, лимит размера запроса,
  защищённый `X-Forwarded-For`, проверка сертификата портала, журнал. Отвечает только на внешнее имя портала:
  запросы с другим именем закрываются без ответа. Порядок — go-live.md.

**Обновление с 0.1.0**

- Миграции применяются автоматически. Агент 0.1.0 продолжает работать; сведения об оборудовании присылает
  агент 0.2.0.

### English

**Hardware**

- Issue, return to stock and transfer a device, with history on its card; "My equipment" for employees.
- CSV catalog import (`;` or `,`, UTF-8): a dry run with per-row errors, then the import; headers in Russian,
  English or Chinese, type-specific fields by key or label.
- "Discovered PCs": PCs with the agent that are not in the catalog are added after an engineer reviews them
  (OS, IP, model and serial number come from the agent) or rejected.
- The agent sends PC hardware and software (SMBIOS, CPU, memory, disks, "Programs and Features") at startup
  and once a day; the PC card shows them, and "Computers" has a "Where is a program installed" search.
- CMDB: "Server" and "Service / IS" types, relations between items (relation types are set by an
  administrator), "What a failure affects" along relations with impact, a dependency map, relation suggestions
  confirmed by an engineer, and a per-type lifecycle (states and allowed transitions).
- Software licenses: purchased seats and expiry; installations are counted from the programs in agent reports,
  and over-used or expiring licenses are flagged.
- NetBox import: devices and virtual machines via an API token once a day; they enter the catalog after an
  engineer reviews them, and accepted cards are not changed automatically.

**Problems and changes**

- New ticket types for engineers: "Problems" (cause, workaround, known error, linked incidents, resolving all
  incidents with one reply) and "Changes" (risk, plan, rollback, work window).
- Change approval by "category + risk level → approver" rules: decisions in "Approvals", a rejection sends the
  change back for rework, editing restarts the approval, only an approved change can be carried out.

**Secrets vault**

- A "Secrets" section for service account passwords and keys: the value is stored encrypted, access is set
  per secret for employees or portal roles (card, value, manage) rather than by AD groups.
- Showing a value is a separate permission, on a button and with a reason; every view is recorded in the
  audit log, the value itself never is.

**Assistant**

- An assistant on your own model behind an OpenAI-compatible API (vLLM, Ollama and others on a separate
  server): answers from the knowledge base with links to articles, a category hint for new tickets, and for
  engineers a reply draft and a ticket summary.
- The model receives only what the user can already see; the conversation is not saved; the assistant only
  advises and does nothing by itself. A superadmin turns it on and sets it up; the API key is stored encrypted.

**Monitoring**

- Alerts from Zabbix, Prometheus Alertmanager and other systems arrive by webhook with a per-source token:
  a notification to superadmins and, above a severity threshold, a ticket in the "Monitoring" channel;
  repeats do not create new tickets, recovery closes the alert.
- Alerts are matched to CMDB items by host (name, IP, PC with the agent); "Service status" on the engineer
  home page also reflects failures of what a service depends on.

**Sign-in and notifications**

- Passwordless domain sign-in (Kerberos) with the "Sign in with domain account" button: turned on in
  "Security", works only from organization networks, the session records the sign-in method.
- Sign-in through an OpenID Connect provider (Keycloak, ADFS, Entra ID): authorization code with PKCE, ID token
  signature check (RS256, ES256), the account comes from the AD mirror by login, UPN or verified email. The
  client secret is stored encrypted. Sign-in from the internet is a separate option and is closed to users who
  need an authenticator code.
- Ticket notifications to Mattermost, Rocket.Chat, Slack (incoming webhook) and Telegram (bot): event, number,
  subject, status and link, without comment texts.

**Phone app and device notifications**

- The portal installs as an app (PWA) on phones and computers: an icon and its own window.
- Device notifications (Web Push), even when the tab is closed. The administrator enables them (System), and
  employees subscribe their own devices in the profile. Messages are encrypted with the browser's key (RFC 8291)
  and signed with the portal's key (VAPID); subscriptions are accepted only for the browsers' push services. By
  default the ticket subject is not included.
- There are no native apps in app stores: the PWA replaces them.

**Domain password**

- Employees change their own AD password under My profile (enabled in security settings, requires LDAPS). The
  portal signs in as the employee with the old password; the domain checks its password policy. A wrong old
  password counts as a failed sign-in; after the change, sessions on other devices are closed.
- The decision on sign-in and sync via Entra ID and Google Workspace and on several domains — ADR-0006.

**Backups and scale**

- How many backups to keep (1–365) and whether to verify each one — under System → Backup. Verification reads
  the dump with `pg_restore --list`; a damaged backup is not kept and super administrators are notified.
- `pult backup` follows these settings and records the result, like the nightly job.
- New document `docs/scale.md` (in Russian): hardware estimates, load testing, several server copies, a
  PostgreSQL cluster, installation without Internet access. Operation at 100,000+ employees is not yet
  confirmed by tests.

**Extensions**

- Extensions are external services described by a manifest (ADR-0005): install from a file, by address or
  from the organization's catalog. The portal shows the requested rights, issues a read-only API token and a
  signed webhook, and adds menu links by rights. A webhook with ticket events requires the extension to ask for
  `helpdesk.ticket.work`. Disabling revokes the token; everything is audited.

**Communications**

- New "Communications" module (off until a superadministrator turns it on): direct messages and group chats.
  Only participants can read them, the text is stored encrypted, messages cannot be deleted, and editing is
  possible for 15 minutes with previous versions kept. The owner and chat administrators manage membership, and
  former members read the history up to the moment they left. Accounts disabled in AD leave group chats, and
  ownership passes to a chat administrator. A superadministrator sees group chats without messages, assigns
  owners and views anonymous statistics.

**Remote support**

- An agent for Linux (systemd): who is signed in (locally, SSH, XRDP), the OS, addresses and inventory, including
  dpkg or rpm packages. Connecting to a Linux session is not supported, and the portal says so. Installation is
  described in the administrator guide, section 10.
- Helper: `register --run-as DOMAIN\name` starts Computer Management and PowerShell through `runas /netonly` with
  the engineer's admin account (a NetOnly equivalent); shadowing asks for that account's credentials.

**Self-service**

- Service catalog: ticket categories as cards grouped by section, with descriptions and search; a card opens the
  request form with that category and its fields. Category descriptions are set in the ticket settings.
- Detailed survey after resolution: optional 1–5 scores for speed, solution quality and courtesy alongside the
  good/bad rating. Average scores and response counts appear in reports and exports.

**Internet publishing**

- `pult edge nginx` prints a ready nginx edge-proxy configuration for a DMZ host: TLS, sign-in and request rate
  limits, agent and helper APIs, metrics and Kerberos closed from outside, a request size limit, a tamper-proof
  `X-Forwarded-For`, verification of the portal's certificate, and a log. It answers only for the portal's
  external name: requests for any other name are closed without a response. See go-live.md.

**Upgrading from 0.1.0**

- Migrations run automatically. Agent 0.1.0 keeps working; hardware details require agent 0.2.0.

### 中文

**设备**

- 设备的发放、退回库存和转交，卡片中保留历史记录；员工可查看“我的设备”。
- 从 CSV 导入目录（`;` 或 `,`，UTF-8）：先试运行并按行显示错误，再正式导入；表头可用俄文、英文或中文，
  类型的附加字段按键名或名称匹配。
- “发现的电脑”：安装了代理但不在目录中的电脑，经工程师审核后加入目录（操作系统、IP、型号和序列号取自代理），
  或予以拒绝。
- 代理在启动时和每天一次发送电脑的硬件和软件信息（SMBIOS、处理器、内存、磁盘、“程序和功能”）；
  电脑卡片显示这些信息，“计算机”页面提供“程序安装在哪些电脑上”搜索。
- CMDB："服务器"和"服务 / 信息系统"类型，对象之间的关系（关系类型由管理员设置），沿有影响的关系分析
  "故障影响范围"，依赖关系图，由工程师确认的关系建议，以及按类型的生命周期（状态和允许的转换）。
- 软件许可证：已购席位和有效期；安装数根据代理报告中的程序统计，超额安装和即将到期的许可证会被标记。
- 从 NetBox 导入：每天通过 API 令牌读取设备和虚拟机；经工程师审核后才进入目录，已接受的卡片不会自动更改。

**问题与变更**

- 面向工程师的新工单类型：“问题”（原因、临时解决方案、已知错误、关联事件、一次回复解决所有事件）和
  “变更”（风险、计划、回退、工作时间窗口）。
- 按“类别 + 风险级别 → 审批人”规则审批变更：在“审批”中决定，拒绝会退回修改，修改会重新开始审批，
  只有已批准的变更才能执行。

**密钥库**

- 用于服务账户密码和密钥的“密钥”栏目：值加密存储，访问权限按每个密钥授予员工或门户角色（卡片、值、管理），
  而不是按 AD 组。
- 显示值是单独的权限，需点击按钮并可填写原因；每次查看都记录在审计日志中，值本身从不写入日志。

**助手**

- 基于 OpenAI 兼容 API 的自有模型助手（部署在独立服务器上的 vLLM、Ollama 等）：根据知识库回答问题并附上
  文章链接，为新工单建议类别，并为工程师生成给申请人的回复草稿和工单摘要。
- 模型只会收到用户本来就能看到的内容；对话不会保存；助手只提供建议，不会自行执行任何操作。由超级管理员
  启用和配置，API 密钥加密存储。

**监控**

- 通过带来源令牌的 Webhook 接收 Zabbix、Prometheus Alertmanager 等系统的告警：通知超级管理员，
  超过严重级别阈值时在“监控”渠道创建工单；重复告警不会创建新工单，恢复后告警关闭。
- 告警按主机（名称、IP、装有代理的电脑）关联到 CMDB 对象；工程师主页上的“服务状态”也会反映服务所依赖对象的故障。

**登录与通知**

- 通过“使用域账户登录”按钮进行无密码域登录（Kerberos）：在“安全”中开启，仅限组织网络，会话记录登录方式。
- 通过 OpenID Connect 提供方登录（Keycloak、ADFS、Entra ID）：使用 PKCE 的授权码流程，校验 ID 令牌签名
  （RS256、ES256），按登录名、UPN 或已验证的邮箱从 AD 镜像中匹配账户。客户端密钥加密存储。从互联网登录需
  单独开启，且需要身份验证器代码的用户无法使用。
- 工单通知发送到 Mattermost、Rocket.Chat、Slack（传入 Webhook）和 Telegram（机器人）：事件、编号、主题、
  状态和链接，不包含评论内容。

**手机应用与设备通知**

- 门户可作为应用（PWA）安装到手机和计算机：有图标和独立窗口。
- 设备通知（Web Push），即使标签页已关闭也能收到。由管理员在“系统”中启用，员工在个人资料中订阅自己的设备。
  消息使用浏览器密钥加密（RFC 8291）并以门户密钥签名（VAPID）；只接受浏览器推送服务的订阅地址。默认不包含工单主题。
- 应用商店中没有原生应用：由 PWA 代替。

**域密码**

- 员工可在“我的资料”中更改自己的 AD 密码（在安全设置中启用，需要 LDAPS）。门户以员工身份用旧密码登录，
  密码策略由域检查。旧密码错误计为一次登录失败；更改后会关闭其他设备上的会话。
- 关于通过 Entra ID 和 Google Workspace 登录与同步以及多个域的决定——见 ADR-0006。

**备份与规模**

- 保留多少个备份（1–365）以及是否校验每个备份——在“系统”→“备份”中设置。校验使用 `pg_restore --list`
  读取转储；损坏的备份不会保留，并通知超级管理员。
- `pult backup` 遵循这些设置并记录结果，与夜间任务相同。
- 新文档 `docs/scale.md`（俄语）：硬件估算、负载测试、多个服务器副本、PostgreSQL 集群、无 Internet 安装。
  100,000 名以上员工规模的运行尚未经过测试确认。

**扩展**

- 扩展是按清单描述的外部服务（ADR-0005）：可从文件、地址或组织目录安装。门户会显示所请求的权限，发放只读
  API 令牌和签名 Webhook，并按权限添加菜单链接。带工单事件的 Webhook 要求扩展申请 `helpdesk.ticket.work` 权限。
  停用会撤销令牌；所有操作都会记入审计。

**沟通**

- 新增“沟通”模块（在超级管理员开启前为关闭状态）：私人对话和群聊。只有参与者可以阅读，文本加密存储，消息不可
  删除，可在 15 分钟内编辑并保留历史版本。成员由所有者和聊天管理员管理，前成员可阅读其退出前的历史记录。
  在 AD 中停用的账户会退出群聊，所有权转交给聊天管理员。超级管理员可查看不含消息的群聊、指定所有者并查看匿名统计。

**远程支持**

- 适用于 Linux（systemd）的代理：谁已登录（本地、SSH、XRDP）、操作系统、地址和资产清单（包括 dpkg 或 rpm 软件包）。
  不支持连接到 Linux 会话，门户会如实提示。安装说明见管理员指南第 10 节。
- Helper：`register --run-as 域\名称` 通过 `runas /netonly` 以工程师的管理员账户启动“计算机管理”和 PowerShell
  （类似 NetOnly），影子连接会询问该账户的凭据。

**自助服务**

- 服务目录：工单类别按分区以卡片显示，带描述和搜索；点击卡片即打开含该类别及其字段的工单表单。类别描述在工单设置中填写。
- 解决后的详细调查：在“好/差”评价之外，可选为速度、解决质量和礼貌打 1–5 分。平均分和答复数显示在报表和导出中。

**发布到互联网**

- `pult edge nginx` 为 DMZ 主机生成现成的 nginx 边缘代理配置：TLS、登录与请求频率限制、对外关闭代理与 helper API、
  指标和 Kerberos、请求大小限制、防伪造的 `X-Forwarded-For`、验证门户证书以及日志。仅响应门户的外部名称：
  其他名称的请求会被直接关闭、不作应答。步骤见 go-live.md。

**从 0.1.0 升级**

- 数据库迁移自动执行。0.1.0 版代理可继续使用；硬件信息需要 0.2.0 版代理。

## 0.1.0 — первый открытый выпуск · first public release · 首个公开版本

### Русский

**Портал**

- Вход паролем Windows: AD по LDAPS или StartTLS, перебор контроллеров, понятные сообщения об отключённой,
  истёкшей и требующей смены пароля учётке.
- Проверка билета Kerberos (MIT GSSAPI по keytab): страница «Состояние» показывает, распознаёт ли портал
  учётку ПК. Это подготовка к входу без пароля; сам вход по билету в 0.1.0 ещё не сделан.
- Второй фактор (TOTP: Яндекс Ключ, Google/Microsoft Authenticator, FreeOTP) по желанию для входа из
  интернета; роли, которым он обязателен, отмечаются в настройках. Неудачи снаружи считаются отдельно.
- Роли и права, площадки, журнал аудита с цепочкой хешей, профиль пользователя (тема, язык, форматы, свои
  сеансы и выход на других устройствах).
- Справочник сотрудников: синхронизация с AD, связывание учёток, поиск с исправлением раскладки, импорт.
- Заявки: категории, шаблоны, SLA по рабочему календарю, назначение по площадкам, вложения, база знаний,
  объявления, повторяющиеся заявки, правила автоматизации, уведомления в портале и по почте.
- Каталог AD и доступы: запросы на доступ с согласованием, операции в AD через отдельную учётку.
- Удалённая поддержка: агент на ПК (сеансы пользователей, подписанные обновления), helper инженера.
- Телефония: журнал звонков, звонок из справочника.
- Интерфейс, письма и ошибки на русском, английском и китайском.

**Установка и эксплуатация**

- Пакет `.deb` для Debian 13 и Ubuntu 24.04+: ставит PostgreSQL, создаёт базу, ключ шифрования и временный
  сертификат, запускает службу; обновление с резервной копией, миграциями одной транзакцией и откатом.
- `install.sh`: проверка сервера перед установкой (система, порт 443, имя, время, PostgreSQL, LDAPS
  контроллеров, контрольная сумма пакета).
- Docker Compose (`deploy/compose.yml`).
- Мастер первого запуска в браузере: домен, администратор, организация.
- Сертификат портала из админки или файлами, вторая пара для другого имени (SNI), работа за обратным прокси.
- Ежедневные резервные копии без ключа шифрования, метрики Prometheus.
- Агент для Windows — MSI; выпуск собирается из тега в GitHub Actions.

### English

**Portal**

- Sign-in with the Windows password: AD over LDAPS or StartTLS, controller failover, clear messages for a
  disabled or expired account and for an account that must change its password.
- Kerberos ticket check (MIT GSSAPI with a keytab): the "Status" page shows whether the portal recognizes the
  PC account. This is groundwork for sign-in without a password; ticket-based sign-in itself is not
  implemented in 0.1.0.
- Optional second factor (TOTP: Yandex Key, Google/Microsoft Authenticator, FreeOTP) for sign-in from the
  internet; the roles that require it are marked in the settings. Failures from outside are counted separately.
- Roles and permissions, sites, an audit log with a hash chain, a user profile (theme, language, formats, own
  sessions and signing out on other devices).
- Staff directory: synchronization with AD, account linking, search that corrects the keyboard layout, import.
- Tickets: categories, templates, SLA on a working calendar, assignment by site, attachments, knowledge base,
  announcements, recurring tickets, automation rules, notifications in the portal and by e-mail.
- AD directory and access: access requests with approval, AD operations through a separate account.
- Remote support: PC agent (user sessions, signed updates), engineer helper.
- Telephony: call log, click-to-call from the directory.
- Interface, e-mails and error messages in Russian, English and Chinese.

**Installation and operations**

- `.deb` package for Debian 13 and Ubuntu 24.04+: installs PostgreSQL, creates the database, the encryption
  key and a temporary certificate, starts the service; updates with a backup, migrations in one transaction
  and rollback.
- `install.sh`: server check before installation (system, port 443, name, time, PostgreSQL, controller LDAPS,
  package checksum).
- Docker Compose (`deploy/compose.yml`).
- First-run wizard in the browser: domain, administrator, organization.
- Portal certificate from the admin panel or from files, a second pair for another name (SNI), operation
  behind a reverse proxy.
- Daily backups without the encryption key, Prometheus metrics.
- Windows agent as an MSI; releases are built from a tag in GitHub Actions.

### 中文

**门户**

- 使用 Windows 密码登录：通过 LDAPS 或 StartTLS 连接 AD，多个域控制器自动切换，对已禁用、已过期以及
  需要修改密码的账户给出清晰提示。
- Kerberos 票据检查（MIT GSSAPI，使用 keytab）："状态"页面显示门户能否识别电脑账户。这是为免密码登录
  所做的准备；基于票据的登录本身在 0.1.0 中尚未实现。
- 从互联网登录时可选第二因素（TOTP：Yandex Key、Google/Microsoft Authenticator、FreeOTP）；必须使用它的
  角色在设置中标记。来自外部的登录失败单独计数。
- 角色与权限、站点、带哈希链的审计日志、用户个人资料（主题、语言、格式、自己的会话以及在其他设备上
  退出登录）。
- 员工通讯录：与 AD 同步、账户关联、可自动纠正键盘布局的搜索、导入。
- 工单：类别、模板、按工作日历计算的 SLA、按站点分配、附件、知识库、公告、周期性工单、自动化规则、
  门户内通知和邮件通知。
- AD 目录与访问权限：带审批的访问申请，通过独立账户执行 AD 操作。
- 远程支持：电脑代理程序（用户会话、签名更新），工程师辅助程序。
- 电话系统：通话记录，从通讯录直接拨号。
- 界面、邮件和错误信息提供俄语、英语和中文。

**安装与运维**

- 适用于 Debian 13 和 Ubuntu 24.04+ 的 `.deb` 软件包：安装 PostgreSQL，创建数据库、加密密钥和临时证书，
  启动服务；更新时先备份，在单个事务中执行迁移，失败时回滚。
- `install.sh`：安装前检查服务器（系统、443 端口、名称、时间、PostgreSQL、域控制器 LDAPS、软件包校验和）。
- Docker Compose（`deploy/compose.yml`）。
- 浏览器中的首次运行向导：域、管理员、组织。
- 门户证书可从管理界面或文件设置，可为另一个名称配置第二对证书（SNI），支持在反向代理后运行。
- 不含加密密钥的每日备份，Prometheus 指标。
- Windows 代理程序以 MSI 提供；发布版本由 GitHub Actions 根据标签构建。
