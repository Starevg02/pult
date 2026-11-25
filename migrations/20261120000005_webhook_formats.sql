-- Уведомления о заявках в мессенджеры через те же вебхуки:
--   pult     — JSON с подписью HMAC (как раньше);
--   chat     — входящий вебхук Mattermost, Rocket.Chat, Slack: {"text": "…"};
--   telegram — Bot API sendMessage: токен бота — в `secret` (зашифрован), чат — в `chat_id`.
ALTER TABLE webhooks
    ADD COLUMN format  text NOT NULL DEFAULT 'pult' CHECK (format IN ('pult', 'chat', 'telegram')),
    ADD COLUMN chat_id text NULL CHECK (chat_id IS NULL OR length(chat_id) BETWEEN 1 AND 100);
