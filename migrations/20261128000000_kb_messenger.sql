-- База знаний: вложения статей. Файлы лежат в общей таблице files (как вложения заявок); файл
-- принадлежит одной статье. Видит вложение тот, кому видна статья.
CREATE TABLE kb_attachments (
    article_id uuid        NOT NULL REFERENCES kb_articles (id) ON DELETE CASCADE,
    file_id    uuid        NOT NULL REFERENCES files (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (article_id, file_id)
);
CREATE UNIQUE INDEX kb_attachments_file ON kb_attachments (file_id);

-- Сообщения: ответ на сообщение того же чата и пересылка (автор исходного сообщения).
ALTER TABLE chat_messages
    ADD COLUMN reply_to       uuid NULL REFERENCES chat_messages (id),
    ADD COLUMN forwarded_from uuid NULL REFERENCES users (id);

-- Реакции: эмодзи из короткого списка, по одной каждого вида от сотрудника.
CREATE TABLE chat_reactions (
    message_id uuid        NOT NULL REFERENCES chat_messages (id),
    user_id    uuid        NOT NULL REFERENCES users (id),
    emoji      text        NOT NULL CHECK (length(emoji) BETWEEN 1 AND 16),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, user_id, emoji)
);

-- Закреплённое сообщение чата (одно) и «без звука» у участника.
ALTER TABLE chats ADD COLUMN pinned_message_id uuid NULL REFERENCES chat_messages (id);
ALTER TABLE chat_members ADD COLUMN muted boolean NOT NULL DEFAULT false;

-- Срок хранения вложений (настройка администратора): у устаревшего файла содержимое стирается,
-- в переписке остаётся отметка «удалён по сроку хранения». Срок считается от kept_since, если он есть:
-- у пересланной копии это время оригинала (пересылка не продлевает хранение).
ALTER TABLE chat_files
    ADD COLUMN expired_at timestamptz NULL,
    ADD COLUMN kept_since timestamptz NULL;
CREATE INDEX chat_files_expiry ON chat_files ((coalesce(kept_since, created_at)))
    WHERE message_id IS NOT NULL AND expired_at IS NULL;
