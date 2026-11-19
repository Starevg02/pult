-- Календари, SLA, матрица приоритета, VIP, группы исполнителей, правила автоматизации,
-- напоминания по «Ждёт ответа», оценка.

-- Рабочий календарь: часы по дням недели ([{day, start, end}]) и особые дни.
CREATE TABLE calendars (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name       text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    hours      jsonb       NOT NULL DEFAULT '[]',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX calendars_name ON calendars (lower(name));

-- Праздник — без часов; перенос или сокращённый день — с часами.
CREATE TABLE calendar_days (
    calendar_id uuid NOT NULL REFERENCES calendars (id) ON DELETE CASCADE,
    day         date NOT NULL,
    start_time  time NULL,
    end_time    time NULL,
    note        text NULL,
    PRIMARY KEY (calendar_id, day),
    CHECK ((start_time IS NULL) = (end_time IS NULL))
);

ALTER TABLE sites ADD COLUMN calendar_id uuid NULL REFERENCES calendars (id) ON DELETE SET NULL;

CREATE TABLE sla_policies (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name             text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    site_id          uuid        NULL REFERENCES sites (id) ON DELETE CASCADE,
    category_id      uuid        NULL REFERENCES ticket_categories (id) ON DELETE CASCADE,
    priority         text        NULL CHECK (priority IN ('low', 'normal', 'high', 'critical')),
    response_minutes integer     NULL CHECK (response_minutes > 0),
    resolve_minutes  integer     NULL CHECK (resolve_minutes > 0),
    sort             integer     NOT NULL DEFAULT 100,
    is_active        boolean     NOT NULL DEFAULT true,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- Группы исполнителей: очередь группы и участники.
CREATE TABLE engineer_groups (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name        text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description text        NULL,
    -- Назначать наименее загруженному участнику сразу.
    auto_assign boolean     NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX engineer_groups_name ON engineer_groups (lower(name));
CREATE TABLE engineer_group_members (
    group_id uuid NOT NULL REFERENCES engineer_groups (id) ON DELETE CASCADE,
    user_id  uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);
CREATE INDEX engineer_group_members_user ON engineer_group_members (user_id);

ALTER TABLE tickets
    ADD COLUMN impact             text        NOT NULL DEFAULT 'person' CHECK (impact IN ('person', 'department', 'organization')),
    -- Срочность ставит инженер (заявитель её не выбирает): вместе с влиянием даёт приоритет по матрице.
    ADD COLUMN urgency            text        NOT NULL DEFAULT 'normal' CHECK (urgency IN ('normal', 'hinders', 'blocked')),
    ADD COLUMN group_id           uuid        NULL REFERENCES engineer_groups (id) ON DELETE SET NULL,
    ADD COLUMN sla_policy_id      uuid        NULL REFERENCES sla_policies (id) ON DELETE SET NULL,
    ADD COLUMN response_due_at    timestamptz NULL,
    ADD COLUMN response_warn_at   timestamptz NULL,
    ADD COLUMN resolve_due_at     timestamptz NULL,
    ADD COLUMN resolve_warn_at    timestamptz NULL,
    -- Пауза SLA: с какого момента и сколько рабочих минут уже стояли.
    ADD COLUMN sla_paused_at      timestamptz NULL,
    ADD COLUMN sla_paused_minutes integer     NOT NULL DEFAULT 0,
    ADD COLUMN sla_warned_at      timestamptz NULL,
    ADD COLUMN sla_breached_at    timestamptz NULL,
    -- «Ждёт ответа»: с какого момента и когда напомнили.
    ADD COLUMN waiting_since      timestamptz NULL,
    ADD COLUMN reminded_at        timestamptz NULL;
CREATE INDEX tickets_sla ON tickets (resolve_warn_at)
    WHERE status IN ('new', 'assigned', 'in_progress', 'waiting', 'waiting_approval', 'on_hold');
CREATE INDEX tickets_waiting ON tickets (waiting_since) WHERE status = 'waiting';
CREATE INDEX tickets_group ON tickets (group_id) WHERE group_id IS NOT NULL;

-- Руководство: приоритет на ступень выше (настройка).
ALTER TABLE persons ADD COLUMN is_vip boolean NOT NULL DEFAULT false;

CREATE TABLE automation_rules (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name       text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    trigger    text        NOT NULL CHECK (trigger IN ('created', 'updated', 'commented', 'sla_warning', 'sla_breached')),
    conditions jsonb       NOT NULL DEFAULT '[]',
    actions    jsonb       NOT NULL DEFAULT '[]',
    is_active  boolean     NOT NULL DEFAULT true,
    stop       boolean     NOT NULL DEFAULT false,
    sort       integer     NOT NULL DEFAULT 100,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX automation_rules_trigger ON automation_rules (trigger, sort) WHERE is_active;

-- Оценка после решения: одна на заявку, заявитель может изменить.
CREATE TABLE ticket_ratings (
    ticket_id  uuid        PRIMARY KEY REFERENCES tickets (id) ON DELETE CASCADE,
    value      text        NOT NULL CHECK (value IN ('up', 'down')),
    comment    text        NULL,
    rated_by   uuid        NULL REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
