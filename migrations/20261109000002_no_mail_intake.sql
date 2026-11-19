-- Решение владельца 09.11.2026: сотрудник взаимодействует с порталом только в браузере.
-- Почта — только уведомления, приёма писем нет.
DROP TABLE email_ingest;
ALTER TABLE ticket_comments DROP COLUMN email_message_id;
DELETE FROM settings WHERE key IN ('imap', 'imap_secret');
