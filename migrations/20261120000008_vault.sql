-- Хранилище секретов: учётки служб, инвентаризации и мониторинга. Значение зашифровано ключом портала
-- (PULT_SECRET_KEY); имя, логин и адрес — открыто, по ним ищут. Права — на каждый секрет, пользователям
-- и ролям портала (не группам AD): просмотр карточки, просмотр значения, управление.
CREATE TABLE vault_secrets (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name             text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    login            text        NOT NULL DEFAULT '' CHECK (length(login) <= 200),
    url              text        NOT NULL DEFAULT '' CHECK (length(url) <= 500),
    description      text        NOT NULL DEFAULT '' CHECK (length(description) <= 2000),
    sealed           bytea       NOT NULL,
    version          integer     NOT NULL DEFAULT 1,
    -- Версия списка доступа: два управляющих не затирают правки друг друга.
    access_version   integer     NOT NULL DEFAULT 1,
    created_by       uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    value_changed_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX vault_secrets_name ON vault_secrets (lower(name));

CREATE TABLE vault_access (
    secret_id uuid NOT NULL REFERENCES vault_secrets (id) ON DELETE CASCADE,
    user_id   uuid NULL REFERENCES users (id) ON DELETE CASCADE,
    role_id   uuid NULL REFERENCES roles (id) ON DELETE CASCADE,
    level     text NOT NULL CHECK (level IN ('view', 'reveal', 'manage')),
    CHECK ((user_id IS NULL) <> (role_id IS NULL)),
    UNIQUE NULLS NOT DISTINCT (secret_id, user_id, role_id)
);
CREATE INDEX vault_access_user ON vault_access (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX vault_access_role ON vault_access (role_id) WHERE role_id IS NOT NULL;
