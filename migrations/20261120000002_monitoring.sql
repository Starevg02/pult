-- Входящие оповещения мониторинга (Prometheus Alertmanager, Zabbix, любой источник с POST JSON).
-- У каждого источника свой токен; хранится только его хеш SHA-256.
CREATE TABLE alert_sources (
    id             uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name           text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    token_hash     bytea       NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    -- Заявка на новое оповещение не ниже этой важности; уведомление суперадминистраторам — всегда.
    create_tickets boolean     NOT NULL DEFAULT false,
    min_severity   text        NOT NULL DEFAULT 'warning' CHECK (min_severity IN ('info', 'warning', 'critical')),
    category_id    uuid        NULL REFERENCES ticket_categories (id) ON DELETE SET NULL,
    site_id        uuid        NULL REFERENCES sites (id) ON DELETE SET NULL,
    is_active      boolean     NOT NULL DEFAULT true,
    last_alert_at  timestamptz NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE alerts (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    source_id   uuid        NOT NULL REFERENCES alert_sources (id) ON DELETE CASCADE,
    -- Отпечаток от источника (fingerprint Alertmanager, id события Zabbix): по нему «решено»
    -- находит своё оповещение, а повтор не создаёт новое.
    fingerprint text        NOT NULL CHECK (length(fingerprint) BETWEEN 1 AND 200),
    title       text        NOT NULL CHECK (length(title) BETWEEN 1 AND 300),
    severity    text        NOT NULL CHECK (severity IN ('info', 'warning', 'critical')),
    host        text        NULL CHECK (length(host) <= 200),
    description text        NULL CHECK (length(description) <= 4000),
    status      text        NOT NULL DEFAULT 'firing' CHECK (status IN ('firing', 'resolved')),
    ticket_id   uuid        NULL REFERENCES tickets (id) ON DELETE SET NULL,
    -- Заявка создаётся: короткая отметка вместо блокировки строки на всё время создания.
    ticket_claimed_at timestamptz NULL,
    started_at  timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX alerts_firing ON alerts (source_id, fingerprint) WHERE status = 'firing';
CREATE INDEX alerts_open ON alerts (started_at DESC) WHERE status = 'firing';
CREATE INDEX alerts_closed ON alerts (started_at DESC) WHERE status = 'resolved';
-- Пределы за час на каждый приём оповещений.
CREATE INDEX alerts_source_started ON alerts (source_id, started_at DESC);
CREATE INDEX alerts_resolved_at ON alerts (resolved_at) WHERE status = 'resolved';
CREATE INDEX alerts_ticket ON alerts (ticket_id) WHERE ticket_id IS NOT NULL;

ALTER TABLE tickets DROP CONSTRAINT tickets_channel_check;
ALTER TABLE tickets ADD CONSTRAINT tickets_channel_check
    CHECK (channel IN ('portal', 'email', 'phone', 'walk_in', 'login_help', 'api', 'monitoring'));
