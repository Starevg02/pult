-- Мастер настройки организации. Установка, где уже есть пользователи, его прошла:
-- мастер показывается только новой установке.
INSERT INTO settings (key, value)
SELECT 'org', '{"setup_done": true}'::jsonb
 WHERE EXISTS (SELECT 1 FROM users)
ON CONFLICT (key) DO NOTHING;
