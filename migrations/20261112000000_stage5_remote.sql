-- Удалённая поддержка — агенты на рабочих станциях, сессии пользователей,
-- политика подключения, подключения инженеров (mstsc /shadow через helper), пакеты агента и helper.
-- Токены и ключи хранятся только как SHA-256: сами значения показываются один раз.

-- Токен регистрации агентов: многоразовый со сроком (вопрос 19: по умолчанию 30 дней).
CREATE TABLE agent_enrollment_tokens (
    id           uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name         text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    token_hash   bytea       NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    -- Первые символы токена — чтобы администратор узнал его в списке.
    prefix       text        NOT NULL,
    created_by   uuid        NOT NULL REFERENCES users (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    revoked_at   timestamptz NULL,
    uses         integer     NOT NULL DEFAULT 0,
    last_used_at timestamptz NULL
);

-- Рабочая станция с агентом. Имя — короткое имя ПК (NetBIOS), без учёта регистра уникально.
CREATE TABLE devices (
    id                  uuid        PRIMARY KEY DEFAULT uuid_v7(),
    hostname            text        NOT NULL CHECK (length(hostname) BETWEEN 1 AND 63),
    fqdn                text        NULL,
    domain_name         text        NULL,
    ad_computer_id      uuid        NULL REFERENCES ad_computers (id) ON DELETE SET NULL,
    os                  text        NULL,
    os_version          text        NULL,
    agent_version       text        NULL,
    ip_addrs            inet[]      NOT NULL DEFAULT '{}',
    -- Адрес, с которого пришёл последний отчёт.
    last_ip             inet        NULL,
    agent_key_hash      bytea       NULL UNIQUE CHECK (length(agent_key_hash) = 32),
    enrollment_token_id uuid        NULL REFERENCES agent_enrollment_tokens (id) ON DELETE SET NULL,
    enrolled_at         timestamptz NOT NULL DEFAULT now(),
    last_seen_at        timestamptz NULL,
    is_active           boolean     NOT NULL DEFAULT true,
    -- Пакет обновления, о котором агент сообщил, что поставил (или версия из отчёта).
    updated_package_id  uuid        NULL
);
CREATE UNIQUE INDEX devices_hostname ON devices (lower(hostname));
CREATE INDEX devices_seen ON devices (last_seen_at DESC);

-- Сессии на ПК — только текущее состояние из последнего отчёта (история не копится).
CREATE TABLE device_sessions (
    device_id   uuid        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    session_id  integer     NOT NULL CHECK (session_id > 0),
    account_id  uuid        NULL REFERENCES ad_accounts (id) ON DELETE SET NULL,
    username    text        NOT NULL,
    user_domain text        NULL,
    state       text        NOT NULL CHECK (state IN ('active', 'disconnected', 'idle')),
    station     text        NULL,
    client_name text        NULL,
    logon_at    timestamptz NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (device_id, session_id)
);
CREATE INDEX device_sessions_account ON device_sessions (account_id) WHERE account_id IS NOT NULL;
CREATE INDEX device_sessions_username ON device_sessions (lower(username));

-- Политика подключения (перенос policy.json): шаблон имени ПК, `*` — любые символы.
CREATE TABLE remote_policies (
    id             uuid        PRIMARY KEY DEFAULT uuid_v7(),
    device_pattern text        NOT NULL CHECK (length(device_pattern) BETWEEN 1 AND 253),
    rule           text        NOT NULL CHECK (rule IN ('deny', 'consent_required')),
    note           text        NOT NULL DEFAULT '',
    created_by     uuid        NULL REFERENCES users (id),
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX remote_policies_pattern ON remote_policies (lower(device_pattern), rule);

-- Подключение инженера: создаётся порталом после проверки прав и политики, токен одноразовый (60 с).
CREATE TABLE remote_connections (
    id                uuid        PRIMARY KEY DEFAULT uuid_v7(),
    operator_id       uuid        NOT NULL REFERENCES users (id),
    device_id         uuid        NOT NULL REFERENCES devices (id),
    action            text        NOT NULL CHECK (action IN ('shadow', 'files', 'manage', 'powershell', 'probe')),
    target_session_id integer     NULL CHECK ((action = 'shadow') = (target_session_id IS NOT NULL)),
    target_user       text        NULL,
    target_account_id uuid        NULL REFERENCES ad_accounts (id) ON DELETE SET NULL,
    mode              text        NULL CHECK (mode IN ('view', 'control')),
    consent           text        NULL CHECK (consent IN ('prompted', 'skipped')),
    CHECK ((action = 'shadow') = (mode IS NOT NULL AND consent IS NOT NULL)),
    CHECK (consent IS DISTINCT FROM 'skipped' OR mode = 'control'),
    ticket_id         uuid        NULL REFERENCES tickets (id) ON DELETE SET NULL,
    -- Адрес, который получит helper (FQDN из AD или имя ПК).
    computer          text        NOT NULL,
    token_hash        bytea       NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    status            text        NOT NULL DEFAULT 'issued'
                                  CHECK (status IN ('issued', 'opened', 'started', 'failed')),
    error             text        NULL,
    -- Итог быстрых действий (проверка доступности: ping и порты).
    result            jsonb       NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    opened_at         timestamptz NULL,
    finished_at       timestamptz NULL
);
CREATE INDEX remote_connections_device ON remote_connections (device_id, created_at DESC);
CREATE INDEX remote_connections_operator ON remote_connections (operator_id, created_at DESC);
CREATE INDEX remote_connections_ticket ON remote_connections (ticket_id) WHERE ticket_id IS NOT NULL;

-- Пакеты: MSI агента и helper (для скачивания из портала), exe агента для обновления по команде.
-- Подпись обновления (Ed25519) делается вне сервера; агент проверяет её своим ключом.
CREATE TABLE agent_packages (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    kind        text        NOT NULL CHECK (kind IN ('agent_msi', 'helper', 'agent_update')),
    version     text        NOT NULL CHECK (length(version) BETWEEN 1 AND 50),
    file_name   text        NOT NULL CHECK (length(file_name) BETWEEN 1 AND 200),
    sha256      bytea       NOT NULL CHECK (length(sha256) = 32),
    size        bigint      NOT NULL CHECK (size > 0),
    signature   bytea       NULL CHECK (signature IS NULL OR length(signature) = 64),
    CHECK (kind <> 'agent_update' OR signature IS NOT NULL),
    data        bytea       NOT NULL,
    is_current  boolean     NOT NULL DEFAULT false,
    uploaded_by uuid        NOT NULL REFERENCES users (id),
    uploaded_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX agent_packages_current ON agent_packages (kind) WHERE is_current;

-- Избранное и псевдонимы ПК — личные настройки инженера.
CREATE TABLE remote_favorites (
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id  uuid        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    alias      text        NOT NULL DEFAULT '' CHECK (length(alias) <= 100),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, device_id)
);

-- Заявка связана с устройством, с которого (по данным агента) работает заявитель.
ALTER TABLE tickets ADD COLUMN device_id uuid NULL REFERENCES devices (id) ON DELETE SET NULL;
CREATE INDEX tickets_device ON tickets (device_id, created_at DESC) WHERE device_id IS NOT NULL;
