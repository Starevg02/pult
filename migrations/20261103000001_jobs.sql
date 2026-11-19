-- Очередь фоновых задач (ADR-0004): своя таблица + FOR UPDATE SKIP LOCKED.
-- Задача ставится в той же транзакции, что и бизнес-данные (заявка и письмо о ней — атомарно).
CREATE TABLE jobs (
    id               uuid        PRIMARY KEY,
    kind             text        NOT NULL,
    payload          jsonb       NOT NULL DEFAULT '{}'::jsonb,
    status           text        NOT NULL DEFAULT 'queued'
                                 CHECK (status IN ('queued', 'running', 'done', 'failed')),
    attempts         integer     NOT NULL DEFAULT 0,
    max_attempts     integer     NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    run_at           timestamptz NOT NULL DEFAULT now(),
    locked_by        text        NULL,
    locked_until     timestamptz NULL,
    last_error       text        NULL,
    -- Повторная постановка с тем же ключом не создаёт вторую задачу (операции каталога AD, письма).
    idempotency_key  text        NULL UNIQUE,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX jobs_ready ON jobs (run_at) WHERE status = 'queued';
CREATE INDEX jobs_leased ON jobs (locked_until) WHERE status = 'running';
