-- Решение владельца 09.11.2026: срочность и удобный способ связи заявитель не выбирает.
-- Приоритет задаёт система (по категории), дальше — инженер.
ALTER TABLE tickets DROP COLUMN urgency;
ALTER TABLE tickets DROP COLUMN contact_method;
