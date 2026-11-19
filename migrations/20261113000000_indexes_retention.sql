-- Индексы после проверки производительности (13.11.2026): внешние ключи и частые фильтры, по которым шёл полный
-- просмотр таблицы. Новая миграция — применённые не меняются.

-- Учётка по карточке человека (имя автора, язык письма заявителю).
CREATE INDEX IF NOT EXISTS users_person ON users (person_id) WHERE person_id IS NOT NULL;
-- «Инженер ли пользователь» — на каждом запросе заявок (первичный ключ начинается с site_id).
CREATE INDEX IF NOT EXISTS site_engineers_user ON site_engineers (user_id);
-- Погашение токенов из писем при закрытии и решении заявки.
CREATE INDEX IF NOT EXISTS ticket_tokens_ticket ON ticket_tokens (ticket_id) WHERE used_at IS NULL;
-- Ответ на закрытую заявку: уже открытое повторное обращение.
CREATE INDEX IF NOT EXISTS tickets_follow_up ON tickets (follow_up_of) WHERE follow_up_of IS NOT NULL;
-- Отчёты и фильтры списка заявок.
CREATE INDEX IF NOT EXISTS tickets_created ON tickets (created_at DESC);
CREATE INDEX IF NOT EXISTS tickets_resolved ON tickets (resolved_at) WHERE resolved_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS tickets_site ON tickets (site_id) WHERE site_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS tickets_category ON tickets (category_id) WHERE category_id IS NOT NULL;
-- Справочник: люди площадки и должности.
CREATE INDEX IF NOT EXISTS persons_site ON persons (site_id) WHERE site_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS persons_position ON persons (position_id) WHERE position_id IS NOT NULL;
-- Имя ПК по адресу (агент, удалённая поддержка): `addresses @> ARRAY[адрес]`.
CREATE INDEX IF NOT EXISTS ad_computers_addresses ON ad_computers USING gin (addresses);
-- Очистка по сроку хранения.
CREATE INDEX IF NOT EXISTS login_attempts_ts ON login_attempts (ts);
CREATE INDEX IF NOT EXISTS outbox_emails_created ON outbox_emails (created_at);
CREATE INDEX IF NOT EXISTS ad_sync_runs_started ON ad_sync_runs (started_at);
