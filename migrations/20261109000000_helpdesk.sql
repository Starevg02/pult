-- Helpdesk.

ALTER TABLE sites ADD COLUMN emergency_phone text NULL;

-- Файлы: содержимое лежит на диске под именем sha256, строка — на каждую загрузку.
CREATE TABLE files (
    id            uuid        PRIMARY KEY DEFAULT uuid_v7(),
    sha256        text        NOT NULL,
    size          bigint      NOT NULL,
    mime          text        NOT NULL,
    original_name text        NOT NULL,
    uploaded_by   uuid        NULL REFERENCES users (id),
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE engineer_absences (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    starts_at  timestamptz NOT NULL,
    ends_at    timestamptz NOT NULL,
    reason     text        NULL,
    created_by uuid        NULL REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE INDEX engineer_absences_user ON engineer_absences (user_id, ends_at);

CREATE TABLE ticket_categories (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    parent_id        uuid        NULL REFERENCES ticket_categories (id),
    -- Служебные категории: `account` («Не могу войти»), `unspecified` (письма).
    code             text        NULL UNIQUE,
    name             text        NOT NULL,
    kind             text        NOT NULL DEFAULT 'incident' CHECK (kind IN ('incident', 'request')),
    default_priority text        NOT NULL DEFAULT 'normal' CHECK (default_priority IN ('low', 'normal', 'high', 'critical')),
    form_schema      jsonb       NOT NULL DEFAULT '[]',
    is_active        boolean     NOT NULL DEFAULT true,
    sort             integer     NOT NULL DEFAULT 0,
    version          integer     NOT NULL DEFAULT 1,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER ticket_categories_touch BEFORE UPDATE ON ticket_categories FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

INSERT INTO ticket_categories (name, code, sort) VALUES
    ('Компьютер и периферия', NULL, 10),
    ('Принтеры и сканеры', NULL, 20),
    ('Сеть и Интернет', NULL, 30),
    ('Почта', NULL, 40),
    ('Учётная запись и пароль', 'account', 50),
    ('Программы и 1С', NULL, 60),
    ('Телефония', NULL, 70),
    ('Другое', NULL, 90),
    ('Не указана', 'unspecified', 100);
INSERT INTO ticket_categories (name, kind, sort) VALUES ('Запрос доступа', 'request', 80);

CREATE SEQUENCE ticket_number_seq START 1000;

CREATE TABLE tickets (
    id                 uuid        PRIMARY KEY DEFAULT uuid_v7(),
    number             bigint      NOT NULL UNIQUE DEFAULT nextval('ticket_number_seq'),
    kind               text        NOT NULL DEFAULT 'incident' CHECK (kind IN ('incident', 'request')),
    title              text        NOT NULL CHECK (length(title) BETWEEN 1 AND 300),
    description        text        NOT NULL DEFAULT '',
    status             text        NOT NULL DEFAULT 'new' CHECK (status IN
                           ('new', 'assigned', 'in_progress', 'waiting', 'waiting_approval', 'on_hold', 'resolved', 'closed', 'canceled')),
    priority           text        NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high', 'critical')),
    -- Срочность глазами сотрудника; приоритет выставляет инженер.
    urgency            text        NOT NULL DEFAULT 'normal' CHECK (urgency IN ('normal', 'hinders', 'blocked')),
    category_id        uuid        NULL REFERENCES ticket_categories (id),
    form_answers       jsonb       NOT NULL DEFAULT '{}',
    site_id            uuid        NULL REFERENCES sites (id),
    -- Заявитель: сотрудник справочника (может быть без учётки) и/или пользователь портала без карточки.
    requester_id       uuid        NULL REFERENCES persons (id),
    requester_user_id  uuid        NULL REFERENCES users (id),
    -- «Не могу войти»: что написал обратившийся, пока инженер не подтвердил личность.
    unverified_contact jsonb       NULL,
    created_by         uuid        NULL REFERENCES users (id),
    assignee_id        uuid        NULL REFERENCES users (id),
    line               text        NOT NULL DEFAULT 'l1' CHECK (line IN ('l1', 'l2')),
    channel            text        NOT NULL DEFAULT 'portal' CHECK (channel IN ('portal', 'email', 'phone', 'walk_in', 'login_help', 'api')),
    contact_phone      text        NULL,
    contact_room       text        NULL,
    contact_method     text        NULL CHECK (contact_method IN ('phone', 'email', 'visit')),
    computer_name      text        NULL,
    parent_ticket_id   uuid        NULL REFERENCES tickets (id),
    is_major           boolean     NOT NULL DEFAULT false,
    follow_up_of       uuid        NULL REFERENCES tickets (id),
    assigned_at        timestamptz NULL,
    taken_at           timestamptz NULL,
    first_response_at  timestamptz NULL,
    resolved_at        timestamptz NULL,
    auto_close_at      timestamptz NULL,
    closed_at          timestamptz NULL,
    closed_by          uuid        NULL REFERENCES users (id),
    reopened_count     integer     NOT NULL DEFAULT 0,
    search_tsv         tsvector    GENERATED ALWAYS AS (to_tsvector('russian'::regconfig, title || ' ' || description)) STORED,
    version            integer     NOT NULL DEFAULT 1,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER tickets_touch BEFORE UPDATE ON tickets FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
CREATE INDEX tickets_assignee ON tickets (assignee_id, status);
CREATE INDEX tickets_requester ON tickets (requester_id);
CREATE INDEX tickets_requester_user ON tickets (requester_user_id);
CREATE INDEX tickets_status ON tickets (status, updated_at DESC);
CREATE INDEX tickets_parent ON tickets (parent_ticket_id) WHERE parent_ticket_id IS NOT NULL;
CREATE INDEX tickets_auto_close ON tickets (auto_close_at) WHERE status = 'resolved';
CREATE INDEX tickets_search ON tickets USING gin (search_tsv);

CREATE TABLE ticket_comments (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id        uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    author_id        uuid        NULL REFERENCES users (id),
    -- Имя автора, у которого нет пользователя портала (письмо от сотрудника без учётки).
    author_name      text        NULL,
    visibility       text        NOT NULL CHECK (visibility IN ('public', 'internal')),
    body             text        NOT NULL,
    source           text        NOT NULL DEFAULT 'portal' CHECK (source IN ('portal', 'email', 'system')),
    email_message_id text        NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    edited_at        timestamptz NULL
);
CREATE INDEX ticket_comments_ticket ON ticket_comments (ticket_id, created_at);

CREATE TABLE ticket_attachments (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id  uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    comment_id uuid        NULL REFERENCES ticket_comments (id) ON DELETE CASCADE,
    file_id    uuid        NOT NULL REFERENCES files (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (ticket_id, file_id)
);
CREATE INDEX ticket_attachments_file ON ticket_attachments (file_id);

CREATE TABLE ticket_events (
    id        uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    actor_id  uuid        NULL REFERENCES users (id),
    kind      text        NOT NULL,
    from_val  jsonb       NULL,
    to_val    jsonb       NULL,
    ts        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ticket_events_ticket ON ticket_events (ticket_id, ts);

-- Когда пользователь последний раз открывал заявку: «непрочитанные изменения жирным».
CREATE TABLE ticket_reads (
    user_id   uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    ticket_id uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    read_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, ticket_id)
);

CREATE TABLE announcements (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id  uuid        NULL REFERENCES tickets (id) ON DELETE SET NULL,
    title      text        NOT NULL,
    body       text        NOT NULL DEFAULT '',
    severity   text        NOT NULL DEFAULT 'major' CHECK (severity IN ('info', 'major')),
    -- Пусто — все площадки.
    site_ids   uuid[]      NOT NULL DEFAULT '{}',
    starts_at  timestamptz NOT NULL DEFAULT now(),
    ends_at    timestamptz NULL,
    created_by uuid        NULL REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX announcements_active ON announcements (ends_at);

-- Одноразовые ссылки «Да / Нет» из письма; в БД — только хэш.
CREATE TABLE ticket_tokens (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id  uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    purpose    text        NOT NULL CHECK (purpose IN ('resolve_confirm', 'rate')),
    token_hash bytea       NOT NULL UNIQUE,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE email_ingest (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    message_id  text        NOT NULL UNIQUE,
    mailbox     text        NOT NULL,
    from_addr   text        NULL,
    subject     text        NULL,
    status      text        NOT NULL CHECK (status IN ('ticket_created', 'comment_added', 'ignored', 'rejected', 'error')),
    ticket_id   uuid        NULL REFERENCES tickets (id) ON DELETE SET NULL,
    reason      text        NULL,
    received_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX email_ingest_recent ON email_ingest (received_at DESC);
CREATE INDEX email_ingest_from ON email_ingest (lower(from_addr), received_at DESC);
