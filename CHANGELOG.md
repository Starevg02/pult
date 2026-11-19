# Изменения · Changelog · 变更记录

Формат — [Keep a Changelog](https://keepachangelog.com/ru/1.1.0/), версии — [SemVer](https://semver.org/lang/ru/).
До версии 1.0 возможны несовместимые изменения настроек и API; миграции базы всегда применяются автоматически.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions: [SemVer](https://semver.org/).
Before version 1.0, settings and the API may change incompatibly; database migrations are always applied
automatically.

格式：[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号：[SemVer](https://semver.org/lang/zh-CN/)。
1.0 版本之前，设置和 API 可能出现不兼容的变更；数据库迁移始终自动执行。

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
