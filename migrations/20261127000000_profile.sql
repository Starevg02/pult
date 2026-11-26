-- Профиль для коллег: статус присутствия и свой статус, аватар, «о себе», стена с ответами и отметками.

CREATE TABLE user_profiles (
    user_id      uuid        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    -- Выбранный режим: auto — «в сети» / «нет на месте» по активности; away, dnd — вручную;
    -- invisible — коллеги видят «не в сети».
    presence     text        NOT NULL DEFAULT 'auto' CHECK (presence IN ('auto', 'away', 'dnd', 'invisible')),
    status_emoji text        NULL CHECK (char_length(status_emoji) <= 16),
    status_text  text        NULL CHECK (char_length(status_text) <= 100),
    -- Свой статус пропадает после этого времени.
    status_until timestamptz NULL,
    about        text        NOT NULL DEFAULT '' CHECK (char_length(about) <= 1000),
    -- Кто пишет на стене: все коллеги, только владелец, никто (стена скрыта).
    wall_mode    text        NOT NULL DEFAULT 'everyone' CHECK (wall_mode IN ('everyone', 'me', 'off')),
    -- Последний сигнал открытой вкладки и «нет действий» от неё же.
    seen_at      timestamptz NULL,
    idle         boolean     NOT NULL DEFAULT false,
    updated_at   timestamptz NOT NULL DEFAULT now()
);

-- Аватар хранится в базе: картинка уже уменьшена в браузере, анимированный GIF — как есть (до 1 МБ);
-- копии базы его сохраняют.
CREATE TABLE user_avatars (
    user_id    uuid        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    mime       text        NOT NULL CHECK (mime IN ('image/jpeg', 'image/png', 'image/webp', 'image/gif')),
    data       bytea       NOT NULL CHECK (octet_length(data) <= 1048576),
    sha256     text        NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Стена: записи и ответы на них (один уровень). Удалённая запись остаётся строкой без текста —
-- ответы на неё не теряют места в ленте.
CREATE TABLE wall_posts (
    id           uuid        PRIMARY KEY DEFAULT uuid_v7(),
    wall_user_id uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    author_id    uuid        NOT NULL REFERENCES users (id),
    parent_id    uuid        NULL REFERENCES wall_posts (id) ON DELETE CASCADE,
    body         text        NOT NULL CHECK (char_length(body) <= 2000),
    created_at   timestamptz NOT NULL DEFAULT now(),
    deleted_at   timestamptz NULL,
    deleted_by   uuid        NULL REFERENCES users (id)
);
CREATE INDEX wall_posts_wall ON wall_posts (wall_user_id, created_at DESC) WHERE parent_id IS NULL;
CREATE INDEX wall_posts_parent ON wall_posts (parent_id, created_at) WHERE parent_id IS NOT NULL;
CREATE INDEX wall_posts_author ON wall_posts (author_id, created_at);

CREATE TABLE wall_likes (
    post_id    uuid        NOT NULL REFERENCES wall_posts (id) ON DELETE CASCADE,
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (post_id, user_id)
);
