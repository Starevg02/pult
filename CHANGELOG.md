# Изменения · Changelog · 变更记录

Формат — [Keep a Changelog](https://keepachangelog.com/ru/1.1.0/), версии — [SemVer](https://semver.org/lang/ru/).
До версии 1.0 возможны несовместимые изменения настроек и API; миграции базы всегда применяются автоматически.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions: [SemVer](https://semver.org/).
Before version 1.0, settings and the API may change incompatibly; database migrations are always applied
automatically.

格式：[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号：[SemVer](https://semver.org/lang/zh-CN/)。
1.0 版本之前，设置和 API 可能出现不兼容的变更；数据库迁移始终自动执行。

## 0.4.1 — оформление сообщений, GIF и письма · messages look, GIFs and emails · 消息外观、GIF 与邮件

### Русский

- «Сообщения» в новом оформлении: фон с узором, цветные пузыри с «хвостиком», время в правом нижнем углу
  пузыря, картинки мозаикой без рамки, цветные аватары и имена в группах, крупные эмодзи. Лента больше не
  прыгает, пока догружаются картинки, а на телефоне переписка открывается на весь экран.
- GIF в переписке: личная подборка «Мои GIF» (до 200 GIF, до 10 МБ каждый) — загрузить с компьютера или
  сохранить из чата, отправить одним нажатием.
- Новая панель эмодзи: разделы, поиск по словам на русском и английском, недавние.
- База знаний: таблицы, зачёркнутый текст, списки задач; кнопка «Таблица» в редакторе.
- Письма оформлены карточкой: название организации, номер заявки, цветная полоса по смыслу письма, текст в
  рамке, кнопки. Служебные письма администраторам — в том же оформлении.
- Интерфейс: живее главная, страница «404» и понятная страница ошибки вместо английского «Something went
  wrong!», цветные значки в пустых списках.
- Надёжность: реакция на телефоне — из меню сообщения; поиск и файлы на телефоне закрываются после перехода к
  сообщению; кнопка «Назад», если чат не открылся; GIF не уходит дважды; удаление GIF — с подтверждением;
  подборка GIF ограничена по объёму и частоте пополнения, файл GIF проверяется по полной сигнатуре.

### English

- Messages have a new look: a patterned background, colored bubbles with tails, the time in the bubble's
  bottom-right corner, borderless image mosaics, colored avatars and names in groups, large emoji. The view no
  longer jumps while images load, and on a phone the chat opens full screen.
- GIFs in chats: a personal "My GIFs" collection (up to 200 GIFs, up to 10 MB each) — upload from your computer
  or save from a chat, send with one tap.
- A new emoji panel: categories, search by Russian and English words, recently used.
- Knowledge base: tables, strikethrough, task lists; a "Table" button in the editor.
- Emails are styled as a card: organization name, request number, a color strip by message meaning, the text in
  a frame, buttons. Service emails to administrators use the same design.
- Interface: a livelier home page, a "404" page and a clear error page instead of "Something went wrong!",
  colored icons in empty lists.
- Reliability: react on a phone from the message menu; search and files on a phone close after jumping to a
  message; a Back button if a chat fails to open; a GIF is never sent twice; removing a GIF asks for
  confirmation; the GIF collection is limited in total size and how often it can grow, and a GIF file is
  checked by its full signature.

### 中文

- “消息”采用新外观：带图案的背景、带尾巴的彩色气泡、时间位于气泡右下角、无边框的图片拼贴、群组中彩色的头像和名字、
  大号表情。图片加载时视图不再跳动，在手机上会话全屏打开。
- 会话中的 GIF：个人“我的 GIF”收藏（最多 200 个，每个不超过 10 MB）——可从电脑上传或从会话保存，一键发送。
- 新的表情面板：分类、按俄文和英文关键词搜索、最近使用。
- 知识库：表格、删除线、任务列表；编辑器中新增“表格”按钮。
- 邮件采用卡片样式：组织名称、工单号、按含义区分颜色的色条、带边框的正文、按钮。发给管理员的服务邮件使用同样的样式。
- 界面：首页更生动，新增“404”页面，并以清晰的错误页面取代 “Something went wrong!”，空列表显示彩色图标。
- 可靠性：在手机上可从消息菜单添加回应；在手机上跳转到消息后搜索和文件面板会关闭；会话打开失败时显示“返回”按钮；
  GIF 不会重复发送；移除 GIF 需要确认；GIF 收藏限制总大小和添加频率，并按完整签名校验 GIF 文件。

## 0.4.0 — мессенджер и база знаний · messenger and knowledge base · 消息与知识库

### Русский

- «Сообщения» стали похожи на привычный мессенджер: пузыри, разделители по дням, «Новые сообщения» при входе,
  аватары в группах, крупные эмодзи. Ответ на сообщение с цитатой, пересылка в другой чат (вместе с файлами),
  реакции, закреплённое сообщение сверху, «печатает…», ✓ — отправлено и ✓✓ — прочитано. Лента не прыгает
  вниз, пока читаете старое: внизу кнопка со счётчиком новых.
- Поиск по переписке (последние 5000 сообщений) и все её файлы — фото сеткой и документы, с переходом к
  сообщению. Картинки открываются во весь экран с листанием.
- Поле ввода растёт вместе с текстом, есть выбор эмодзи, ↑ — правка последнего своего сообщения, файлы можно
  бросить на всю переписку. Разметка в сообщениях: **жирный**, _курсив_, ~~зачёркнутый~~, `код` и блоки кода.
- Список переписок: поиск, «Непрочитанные», время последнего сообщения, беззвучные чаты. Звук и уведомления на
  рабочем столе о новых сообщениях (настраиваются в браузере), непрочитанное — в заголовке вкладки.
- Суперадминистратор видит, сколько места занимают переписки, и может задать срок хранения вложений (по
  умолчанию — всегда). Сообщения по-прежнему не удаляются никогда.
- База знаний: статьи оформлены «как в Telegraph» — крупный заголовок, шрифт с засечками, картинки на всю
  ширину с подписью, содержание для длинных статей, время чтения, печать. У статей появились вложения: файлы
  и картинки добавляются вставкой, перетаскиванием или скрепкой. Черновик из заявки забирает её открытые
  вложения.
- Новый редактор статьи: панель оформления (заголовки, списки, цитата, код, ссылка), просмотр рядом с текстом,
  Ctrl+B/I/K/S, предупреждение о несохранённых изменениях. В списке статей — обложка и начало текста.

### English

- Messages now feel like a familiar messenger: bubbles, day separators, a "New messages" marker on entry, avatars
  in groups, large emoji. Reply with a quote, forward to another chat (with files), reactions, a pinned message
  on top, "typing…", ✓ sent and ✓✓ read. The view no longer jumps down while you read older messages: a button
  at the bottom counts new ones.
- Search in a chat (the last 5,000 messages) and all of its files — photos in a grid and documents, with a jump to
  the message. Images open full screen and can be browsed.
- The message box grows with the text, has an emoji picker, ↑ edits your last message, and files can be dropped
  anywhere on the chat. Message formatting: **bold**, _italic_, ~~strikethrough~~, `code` and code blocks.
- Chat list: search, "Unread", last message time, muted chats. Sound and desktop notifications for new messages
  (set per browser); the unread count is in the tab title.
- The superadministrator sees how much space chats take and can set an attachment retention period (default:
  forever). Messages are still never deleted.
- Knowledge base: articles look like Telegraph — a large title, a serif font, full-width images with captions,
  contents for long articles, reading time, print. Articles now have attachments: add files and images by
  pasting, dragging or the paperclip. A draft made from a ticket takes its public attachments.
- New article editor: a formatting toolbar (headings, lists, quote, code, link), side-by-side preview,
  Ctrl+B/I/K/S, a warning about unsaved changes. The article list shows a cover and the start of the text.

### 中文

- “消息”现在像常用的即时通讯工具：气泡、按日期分隔、进入时的“新消息”标记、群组中的头像、大号表情。可引用回复、
  转发到其他会话（含文件）、添加回应、置顶消息、“正在输入…”，✓ 表示已发送，✓✓ 表示已读。阅读旧消息时视图不再跳到底部：
  底部按钮会显示新消息数量。
- 会话内搜索（最近 5000 条消息）以及会话中的所有文件——图片网格和文档，可跳转到对应消息。图片可全屏查看并翻页。
- 输入框随文字增高，带表情选择器，↑ 编辑自己的上一条消息，文件可拖放到整个会话区域。消息格式：**粗体**、_斜体_、
  ~~删除线~~、`代码` 和代码块。
- 会话列表：搜索、“未读”、最后消息时间、静音会话。新消息提示音和桌面通知（按浏览器设置）；未读数显示在标签页标题中。
- 超级管理员可以查看会话占用的空间，并设置附件保留期限（默认永久）。消息仍然永不删除。
- 知识库：文章采用 Telegraph 风格——大标题、衬线字体、带说明的全宽图片、长文目录、阅读时间、打印。文章支持附件：
  可通过粘贴、拖放或回形针添加文件和图片。由工单生成的草稿会带上工单的公开附件。
- 新的文章编辑器：格式工具栏（标题、列表、引用、代码、链接）、并排预览、Ctrl+B/I/K/S、未保存更改提醒。
  文章列表显示封面和正文开头。

## 0.3.2 — лента записей · post feed · 动态

### Русский

- «Лента» в меню: записи со стен всех коллег, новые сверху, с ответами и «Нравится». Новые записи появляются
  сами, без обновления страницы. Свою запись можно опубликовать прямо из ленты. Выключенные стены в ленту не
  попадают.
- Анимированный GIF-аватар уменьшается без ошибки «даже уменьшенный он больше 1 МБ»: в каждый кадр пишется
  только то, что изменилось, одинаковые кадры склеиваются. Если и так больше 1 МБ, сервер пропускает часть
  кадров и уменьшает сторону.
- «Сообщения»: файл, загружающийся при переключении чата или во время правки, больше не теряется; во время
  отправки можно продолжать прикреплять файлы; файл, брошенный мимо поля, не открывается вместо портала; при
  вставке текста из Word вместе с ним не прикрепляется картинка.
- Безопасность и надёжность: загрузка вложений и аватара получает тот же долгий срок, что вложения заявок;
  суточная квота вложений на сотрудника (1 ГБ) и проверка квоты под блокировкой; ограничено число одновременных
  скачиваний; символы смены направления текста убираются из имён файлов, сообщений, статуса и записей;
  уменьшение GIF не обходит предел одновременной работы при оборванном запросе.
- Профиль: смена статуса не сбрасывает его срок; прозрачная картинка аватара — на белом фоне; справочник не
  мигает аватарами при поиске; ответы на стене ограничены для каждой записи, а не для всей страницы.

### English

- Feed in the menu: posts from all colleagues' walls, newest first, with replies and likes. New posts appear on
  their own without reloading. You can post from the feed. Walls that are turned off are not in the feed.
- An animated GIF avatar no longer fails with "even shrunk it is larger than 1 MB": each frame stores only what
  changed and identical frames are merged. If it is still larger than 1 MB, the server drops frames and reduces
  the size.
- Messages: a file still uploading when you switch chats or start editing is no longer lost; you can keep
  attaching files while a message is being sent; a file dropped outside the box no longer replaces the portal
  page; pasting text from Word no longer attaches its preview image.
- Security and reliability: attachment and avatar uploads get the long upload deadline; a daily attachment quota
  per employee (1 GB), checked under a lock; concurrent downloads are limited; text direction override characters
  are removed from file names, messages, statuses and posts; GIF shrinking keeps its concurrency limit when a
  request is aborted.
- Profile: changing the status keeps its expiry; a transparent avatar picture gets a white background; the
  phonebook no longer flickers avatars while searching; wall replies are limited per post, not per page.

### 中文

- 菜单中的“动态”：所有同事留言墙上的帖子，最新的在上方，含回复和点赞。新帖子无需刷新即可自动出现。可以直接在动态中发帖。
  已关闭的留言墙不会出现在动态中。
- 动态 GIF 头像不再出现“缩小后仍大于 1 MB”的错误：每帧只保存变化的部分，相同的帧会合并。若仍大于 1 MB，服务器会丢弃部分帧并缩小尺寸。
- 消息：切换聊天或开始编辑时仍在上传的文件不再丢失；发送过程中可以继续添加附件；拖到输入框外的文件不再替换门户页面；
  从 Word 粘贴文本时不再附带其预览图片。
- 安全与可靠性：附件和头像上传使用较长的上传时限；每位员工每日附件配额（1 GB），在锁内检查；限制同时下载数；
  文件名、消息、状态和帖子中的文字方向控制字符会被移除；请求中断时 GIF 缩小仍遵守并发限制。
- 个人资料：修改状态不会重置其期限；透明头像图片使用白色背景；在通讯录中搜索时头像不再闪烁；留言墙回复按每条帖子限制，而非按整页。

## 0.3.1 — вложения в сообщениях · attachments in Messages · 消息附件

### Русский

- В «Сообщениях» можно прикреплять файлы: кнопка со скрепкой, вставка картинки из буфера или перетаскивание.
  До 10 файлов в сообщении; разрешены те же типы и тот же предел размера, что у вложений заявок
  (настройка Helpdesk «Размер вложения», по умолчанию 25 МБ). Сообщение может состоять только из файлов.
- Картинки показываются прямо в переписке, остальные файлы скачиваются. Имя и содержимое файла хранятся
  зашифрованными ключом портала, как и текст; открыть вложение могут только участники чата (вышедший — только
  отправленные до выхода). Загруженный, но не отправленный файл удаляется через сутки.
- В списке переписок сообщение без текста подписано числом вложений.
- Анимированный GIF-аватар теперь можно загрузить до 15 МБ: сервер обрезает его в квадрат 256 точек и
  сохраняет все кадры (раньше — только до 1 МБ как есть).

### English

- Messages now take attachments: the paperclip button, pasting an image or dropping files. Up to 10 files per
  message; the same file types and size limit as ticket attachments (the Helpdesk "Attachment size" setting,
  25 MB by default). A message may consist of files only.
- Images are shown right in the conversation, other files download. The file name and content are stored
  encrypted with the portal key, like the text; only chat members can open an attachment (a former member, only
  those sent before they left). An uploaded file that is never sent is deleted after a day.
- In the chat list a message without text shows the number of attachments.
- An animated GIF avatar can now be up to 15 MB: the server crops it to a 256-pixel square and keeps all frames
  (before, only up to 1 MB as is).

### 中文

- “消息”支持附件：回形针按钮、粘贴图片或拖入文件。每条消息最多 10 个文件；文件类型和大小限制与工单附件相同
  （Helpdesk 设置“附件大小”，默认 25 MB）。消息可以只包含文件。
- 图片直接显示在对话中，其他文件可下载。文件名和内容与文本一样使用门户密钥加密存储；只有聊天成员可以打开附件
  （已退出的成员只能打开退出前发送的附件）。上传后未发送的文件一天后删除。
- 在会话列表中，没有文字的消息显示附件数量。
- 动态 GIF 头像现在最大可达 15 MB：服务器将其裁成 256 像素正方形并保留所有帧（此前只能原样上传 1 MB 以内的文件）。

## 0.3.0 — профиль сотрудника: статус, аватар, стена · employee profile: status, avatar, wall · 员工主页：状态、头像、留言墙

### Русский

**Присутствие и статус**

- Присутствие, как в мессенджерах: «В сети», «Отошёл», «Не беспокоить», «Не в сети». Открытая вкладка портала
  раз в минуту сообщает, что сотрудник на месте; через 10 минут без действий — «Отошёл». Режим выбирается в
  меню учётной записи: «Автоматически», «Отошёл», «Не беспокоить», «Невидимка» (коллеги видят «Не в сети»).
- «Не беспокоить» не отправляет уведомления на устройства: они остаются в колокольчике.
- Свой статус — значок и текст («В отпуске», «На совещании», «Работаю удалённо» или свой) со сроком: 30 минут,
  час, 4 часа, сегодня, неделя или без срока.

**Аватар и страница сотрудника**

- Аватар загружается в «Мой профиль» → «Профиль для коллег»: картинка обрезается в квадрат и сжимается в
  браузере, анимированный GIF загружается как есть и сохраняет анимацию; сервер принимает только JPEG, PNG,
  WebP и GIF до 1 МБ и проверяет содержимое файла, а не расширение.
- Страница сотрудника (`/people/<id>`): аватар, присутствие, статус, должность, отдел, площадка, контакты из
  справочника, «О себе» и кнопка «Написать».
- Аватары и присутствие — в справочнике, в меню учётной записи, в «Сообщениях» (список, шапка личной
  переписки, участники группы).

**Стена**

- На странице сотрудника — стена: записи коллег, ответы, «Нравится». Владелец выбирает, кто пишет: все
  коллеги, только он сам (коллеги отвечают) или стена выключена.
- Владелец стены получает уведомление о новой записи, автор — об ответе. Не больше 30 записей за 10 минут.
- Новое право «Модерация профилей и стен» (по умолчанию — у суперадминистратора): убрать чужой аватар или
  запись. Действие попадает в журнал аудита.

**Обновление**

- Миграция `20261127000000_profile` добавляет таблицы профиля, аватаров и стены; данные не меняются.

### English

**Presence and status**

- Messenger-style presence: Online, Away, Do not disturb, Offline. An open portal tab reports once a minute
  that the employee is around; after 10 minutes without input they show as Away. The mode is picked in the
  account menu: Automatic, Away, Do not disturb, Invisible (colleagues see Offline).
- Do not disturb stops notifications from going to devices: they stay in the bell.
- Custom status — an emoji and text ("On vacation", "In a meeting", "Working remotely" or your own) with an end
  time: 30 minutes, an hour, 4 hours, today, this week or none.

**Avatar and employee page**

- Upload an avatar in My profile → Profile for colleagues: the browser crops the picture to a square and
  shrinks it, an animated GIF is uploaded as is and keeps its animation; the server accepts only JPEG, PNG,
  WebP and GIF up to 1 MB and checks the file content, not the extension.
- Employee page (`/people/<id>`): avatar, presence, status, position, department, site, phonebook contacts,
  About, and a Message button.
- Avatars and presence appear in the phonebook, the account menu and Messages (list, direct chat header,
  group members).

**Wall**

- The employee page has a wall: colleagues' posts, replies and likes. The owner chooses who can post: all
  colleagues, only the owner (colleagues reply) or nobody (wall off).
- The wall owner is notified about new posts, the author about replies. At most 30 posts per 10 minutes.
- New permission "Moderate profiles and walls" (superadmin by default): remove someone's avatar or post. The
  action is recorded in the audit log.

**Upgrade**

- Migration `20261127000000_profile` adds the profile, avatar and wall tables; existing data is untouched.

### 中文

**在线状态与个人状态**

- 类似即时通讯软件的在线状态：在线、离开、请勿打扰、离线。打开的门户页面每分钟报告一次员工在线；10 分钟无操作后显示为
  “离开”。在账户菜单中选择模式：自动、离开、请勿打扰、隐身（同事看到“离线”）。
- “请勿打扰”时通知不会推送到设备，只保留在铃铛中。
- 自定义状态：图标和文字（“休假中”“开会中”“远程办公”或自定义），可设置结束时间：30 分钟、1 小时、4 小时、今天、本周或不清除。

**头像与员工主页**

- 在“我的资料”→“同事可见的资料”中上传头像：浏览器将图片裁成正方形并压缩，动态 GIF 原样上传并保留动画；服务器只接受
  1 MB 以内的 JPEG、PNG、WebP 和 GIF，并检查文件内容而非扩展名。
- 员工主页（`/people/<id>`）：头像、在线状态、个人状态、职位、部门、站点、通讯录联系方式、“关于我”和“发消息”按钮。
- 头像和在线状态显示在通讯录、账户菜单和“消息”中（列表、私聊标题、群成员）。

**留言墙**

- 员工主页上有留言墙：同事的留言、回复和点赞。主人可以选择谁能发帖：所有同事、仅本人（同事可回复）或关闭留言墙。
- 有新留言时通知墙的主人，有回复时通知作者。每 10 分钟最多 30 条。
- 新权限“管理资料和留言墙”（默认属于超级管理员）：移除他人的头像或留言。该操作记入审计日志。

**升级**

- 迁移 `20261127000000_profile` 新增资料、头像和留言墙的表；现有数据不变。

## 0.2.1 — удобство и исправления после пилота · usability and fixes after the pilot · 试点后的易用性改进与修复

### Русский

**Меню и названия**

- Меню разделено на группы: повседневное сверху, «ИТ-служба», «Справка», «Личное».
- Новые названия: «Сообщения» (было «Переписка», модуль «Коммуникации»), «Очередь заявок» (было «Рабочее
  место»), «Хранилище секретов», «ИИ-помощник», «Active Directory: синхронизация» и «Active Directory:
  изменения» (было «Каталог AD» и «Каталог AD: запись»). В настройках «Почта» — в «Подключениях»,
  «Сообщения» — в «Люди и права». Документация — с теми же названиями.

**ИИ-помощник**

- Запросы к помощнику больше не обрываются через 60 секунд: срок HTTP-запроса считается от тайм-аута
  помощника (предел — 300 секунд) — модели без видеокарты отвечают дольше минуты.
- Страница помощника — как чат: поле ввода внизу, прокрутка, разметка ответа, таймер ожидания и
  «Остановить» (вопрос возвращается в поле). Беседа сохраняется при переходе в другой раздел до перезагрузки
  вкладки; неотвеченный вопрос при уходе прерывается и возвращается в поле.
- «Черновик ответа», «Кратко о заявке» и «Подобрать категорию» — с таймером и «Остановить»; запрос
  прерывается при уходе со страницы. Подсказка категории не перебивает категорию, выбранную вручную, пока
  модель думала. Черновик, запрошенный перед переключением на внутреннюю заметку, не теряется и не
  вставляется дважды.

**Заявки**

- Ответ в решённую или закрытую заявку, который создал новую заявку, открывает её и говорит об этом.
- В отменённой заявке вместо поля ответа — подсказка и ссылка на новую заявку.
- После создания заявки — «Создать ещё»; отправка формы ждёт окончания загрузки вложений.
- Ответы формы категории в карточке — с подписями полей и «Да» / «Нет» вместо ключей и `true` / `false`.
- «Возобновить» для своей заявки в ожидании или отложенной; диалог передачи начинается с текущей линии
  заявки и сбрасывается при закрытии.
- Массовые «Решить» и «Объединить» в очереди — с подтверждением («для 1 заявки», «для 5 заявок»).
- Пока площадок нет, форма новой заявки говорит, что заявку не отправить и где их добавить.
- «Решить все инциденты» проблемы требует код закрытия; сервер проверяет его до начала, а не молча
  отклоняет каждый инцидент.
- Поле ответа подписано для экранного диктора; в карточке заявки подсвечен раздел «Мои заявки».

**Остальное**

- Сообщения: неотправленный текст не теряется при правке другого сообщения и при переходе в другой чат.
- Уведомление отмечается прочитанным при переходе по нему.
- Главная: ошибка загрузки своих заявок видна, счётчик показывает «6+», если заявок больше.
- Поиск по базе знаний без результатов говорит «ничего не нашлось», а не «статей пока нет»; в статьях
  оформлены списки, заголовки, код и цитаты.
- Отчёты: дата «по» по умолчанию — сегодняшняя по местному времени (после полуночи была вчерашней).
- Почта: сохранённый пароль SMTP можно удалить (сервер без входа).
- Подтверждение перед отзывом токена регистрации, рассылкой обновления агентам, отключением ПК, удалением
  правила согласования и повторным запуском мастера настройки.
- Подписи пяти разделов настроек, которых не было в словаре.
- Журналы писем и синхронизации AD показывают статусы словами («В очереди», «Готово»), а не служебными кодами.
- Поиск собеседника в «Сообщениях» без результатов объясняет, что написать можно тому, кто уже входил в портал.
- Адрес `/login` из закладки после входа ведёт на главную, а не на «Страница не найдена».

### English

**Menu and names**

- The menu is split into groups: everyday items on top, then “IT team”, “Help” and “Personal”.
- New names: “Messages” (was the “Communications” module), “Ticket queue” (was “Workspace”), “Secrets
  vault”, “AI assistant”, “Active Directory: sync” and “Active Directory: changes” (was “AD directory” and
  “AD directory: writes”). In settings, “Mail” moved to “Connections” and “Messages” to “People and
  access”. The documentation uses the same names.

**AI assistant**

- Assistant requests are no longer cut off after 60 seconds: the HTTP deadline follows the assistant timeout
  (up to 300 seconds), since CPU-only models take more than a minute to answer.
- The assistant page works like a chat: input at the bottom, scrolling, formatted answers, a wait timer and
  “Stop” (the question goes back to the input). The conversation survives switching sections until the tab
  is reloaded; an unanswered question is aborted on leaving and put back into the input.
- “Draft reply”, “Summary” and “Suggest category” show a timer and “Stop”; the request is aborted when the
  page is left. A category suggestion no longer overrides a category picked by hand while the model was
  thinking. A draft requested before switching to an internal note is neither lost nor inserted twice.

**Tickets**

- A reply to a resolved or closed ticket that created a new ticket now opens it and says so.
- A canceled ticket shows a hint and a link to a new ticket instead of the reply field.
- “Create another” after creating a ticket; the form waits for attachments to finish uploading.
- Category form answers on the ticket card show field labels and “Yes” / “No” instead of keys and
  `true` / `false`.
- “Resume” for your own waiting or on-hold ticket; the transfer dialog starts from the ticket’s current line
  and resets on close.
- Bulk “Resolve” and “Merge” in the queue ask for confirmation, with correct plural forms.
- While there are no sites, the new ticket form says the request cannot be sent and where to add them.
- “Resolve all incidents” of a problem requires a closure code; the server checks it up front instead of
  silently rejecting each incident.
- The reply field has an accessible label; the “My tickets” menu item is highlighted on a ticket card.

**Other**

- Messages: unsent text is kept when editing another message or switching chats.
- A notification is marked as read when it is opened.
- Home: an error loading your tickets is shown, and the counter shows “6+” when there are more.
- A knowledge base search with no results says “nothing found” instead of “no articles yet”; articles render
  lists, headings, code and quotes.
- Reports: the default “to” date is today in local time (after midnight it used to be yesterday).
- Mail: the saved SMTP password can be removed (server without sign-in).
- Confirmation before revoking an enrolment token, rolling out an agent update, deactivating a PC, deleting
  an approval rule and rerunning the setup wizard.
- Labels for five settings sections that were missing from the dictionary.
- The mail and AD sync logs show statuses as words (“Queued”, “Done”) instead of internal codes.
- A people search in Messages with no results explains that you can message those who have signed in before.
- A bookmarked `/login` address leads to the home page after sign-in instead of “Page not found”.

### 中文

**菜单与名称**

- 菜单分组：日常功能在上方，其后为“IT 服务”“帮助”“个人”。
- 新名称：“消息”（原“沟通”模块）、“工单队列”（原“工作台”）、“密钥库”、“AI 助手”、“Active Directory：同步”和
  “Active Directory：变更”（原“AD 目录”和“AD 目录：写入”）。设置中“邮件”移至“连接”，“消息”移至“人员和访问权限”。
  文档使用相同名称。

**AI 助手**

- 助手请求不再在 60 秒后中断：HTTP 时限随助手超时计算（最长 300 秒），无显卡的模型回答需要一分钟以上。
- 助手页面采用聊天形式：输入框在底部、可滚动、回答带格式、显示等待计时和“停止”（问题回到输入框）。切换栏目时对话
  保留，直到刷新标签页；离开时未回答的问题会被中止并放回输入框。
- “回复草稿”“摘要”和“推荐类别”显示计时和“停止”；离开页面时中止请求。模型思考期间手动选择的类别不再被推荐覆盖。
  切换到内部备注前请求的草稿不会丢失，也不会插入两次。

**工单**

- 对已解决或已关闭工单的回复若创建了新工单，现在会打开新工单并给出提示。
- 已取消的工单显示提示和新建工单链接，而不是回复框。
- 创建工单后可“再创建一个”；表单会等待附件上传完成。
- 工单卡片中的类别表单答案显示字段名称和“是/否”，而不是键名和 `true` / `false`。
- 自己的等待中或已挂起工单可“恢复处理”；转交对话框从工单当前线路开始，关闭时重置。
- 队列中的批量“解决”和“合并”需要确认。
- 尚无站点时，新建工单表单会说明无法提交以及在哪里添加站点。
- 问题的“解决全部事件”需要关闭代码；服务器会预先检查，而不是逐个静默拒绝事件。
- 回复框带有无障碍标签；在工单卡片中“我的工单”菜单项保持高亮。

**其他**

- 消息：编辑其他消息或切换聊天时，未发送的文本会保留。
- 打开通知即标记为已读。
- 首页：加载自己的工单出错时会显示，工单较多时计数显示“6+”。
- 知识库搜索无结果时提示“未找到”，而不是“暂无文章”；文章支持列表、标题、代码和引用格式。
- 报表：默认“截止”日期为本地时间的今天（过去午夜后会显示为昨天）。
- 邮件：可删除已保存的 SMTP 密码（无需登录的服务器）。
- 撤销注册令牌、向代理推送更新、停用电脑、删除审批规则和重新运行设置向导前需要确认。
- 补充词典中缺失的五个设置栏目说明。
- 邮件和 AD 同步日志以文字显示状态（“排队中”“完成”），而不是内部代码。
- 在“消息”中搜索联系人无结果时，会说明只能给登录过门户的员工发消息。
- 从书签打开 `/login` 登录后进入首页，而不是“页面未找到”。

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
