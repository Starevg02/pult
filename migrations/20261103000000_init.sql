-- Базовая схема. Миграции только вперёд.
-- Расширения для поиска в справочнике и заявках.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS unaccent;

-- Настройки, которые меняются из админки.
CREATE TABLE settings (
    key         text        PRIMARY KEY,
    value       jsonb       NOT NULL,
    updated_by  uuid        NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
