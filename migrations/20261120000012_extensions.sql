-- Расширения (ADR-0005): внешние сервисы по манифесту. Портал выдаёт расширению токен API с перечисленными
-- в манифесте правами (только чтение), подписанный вебхук на события заявок и ссылки в меню.
-- Код расширения в портале не выполняется.

CREATE TABLE extensions (
    id           uuid        PRIMARY KEY DEFAULT uuid_v7(),
    -- Идентификатор из манифеста: `com.example.reports`.
    ext_id       text        NOT NULL UNIQUE,
    name         text        NOT NULL,
    version      text        NOT NULL,
    manifest     jsonb       NOT NULL,
    -- Откуда установлено: адрес манифеста или NULL (файл).
    source_url   text        NULL,
    token_id     uuid        NULL REFERENCES api_tokens (id) ON DELETE SET NULL,
    webhook_id   uuid        NULL REFERENCES webhooks (id) ON DELETE SET NULL,
    enabled      boolean     NOT NULL DEFAULT true,
    installed_by uuid        NOT NULL REFERENCES users (id),
    installed_at timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
