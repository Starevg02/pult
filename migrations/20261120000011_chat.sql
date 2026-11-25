-- Модуль «Коммуникации»: личные диалоги и групповые чаты.
-- Текст сообщений и их прежних версий хранится зашифрованным ключом портала (как секреты),
-- читать его штатно могут только участники; администраторы портала видят только обезличенную статистику.
-- Сообщения не удаляются (ни автором, ни администратором), правка — автором в течение 15 минут, прежние версии хранятся.

CREATE TABLE chats (
    id              uuid        PRIMARY KEY DEFAULT uuid_v7(),
    kind            text        NOT NULL CHECK (kind IN ('direct', 'group')),
    -- Название группового чата; у личного диалога пусто.
    title           text        NULL,
    -- Личный диалог один на пару: «меньший id:больший id».
    direct_key      text        NULL UNIQUE,
    created_by      uuid        NOT NULL REFERENCES users (id),
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- Последнее сообщение — для сортировки списка.
    last_message_at timestamptz NULL,
    -- Владелец заблокирован в AD, а замены среди администраторов нет: чат ждёт нового владельца.
    locked          boolean     NOT NULL DEFAULT false,
    CHECK ((kind = 'direct') = (direct_key IS NOT NULL)),
    CHECK ((kind = 'group') = (title IS NOT NULL))
);

CREATE TABLE chat_members (
    chat_id      uuid        NOT NULL REFERENCES chats (id),
    user_id      uuid        NOT NULL REFERENCES users (id),
    role         text        NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'admin', 'member')),
    -- С какого момента администратор: при блокировке владельца владельцем становится самый давний.
    admin_since  timestamptz NULL,
    joined_at    timestamptz NOT NULL DEFAULT now(),
    -- Вышел или исключён: читает историю до этого момента, писать не может.
    left_at      timestamptz NULL,
    left_reason  text        NULL CHECK (left_reason IN ('left', 'removed', 'blocked')),
    last_read_at timestamptz NULL,
    pinned       boolean     NOT NULL DEFAULT false,
    -- Скрыт из списка (только бывший групповой чат).
    hidden       boolean     NOT NULL DEFAULT false,
    PRIMARY KEY (chat_id, user_id),
    CHECK ((left_at IS NULL) = (left_reason IS NULL))
);
CREATE INDEX chat_members_user ON chat_members (user_id);

CREATE TABLE chat_messages (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    chat_id    uuid        NOT NULL REFERENCES chats (id),
    author_id  uuid        NOT NULL REFERENCES users (id),
    -- Зашифрованный текст.
    body       bytea       NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    edited_at  timestamptz NULL,
    version    integer     NOT NULL DEFAULT 1
);
CREATE INDEX chat_messages_chat ON chat_messages (chat_id, created_at);

-- Прежние версии отредактированных сообщений.
CREATE TABLE chat_message_versions (
    message_id uuid        NOT NULL REFERENCES chat_messages (id),
    version    integer     NOT NULL,
    body       bytea       NOT NULL,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (message_id, version)
);

-- История состава группового чата (видят владелец и администраторы чата), в ленту не попадает.
CREATE TABLE chat_member_events (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    chat_id    uuid        NOT NULL REFERENCES chats (id),
    user_id    uuid        NOT NULL REFERENCES users (id),
    -- Кто сделал; NULL — система (блокировка учётки в AD).
    actor_id   uuid        NULL REFERENCES users (id),
    action     text        NOT NULL CHECK (action IN ('created', 'added', 'removed', 'left', 'blocked', 'admin', 'member', 'owner', 'locked', 'unlocked')),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX chat_member_events_chat ON chat_member_events (chat_id, created_at);
