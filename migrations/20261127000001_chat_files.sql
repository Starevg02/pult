-- Вложения сообщений. Как и текст, хранятся зашифрованными ключом портала (имя файла тоже): читают
-- только участники чата. Файл загружается заранее и прикрепляется при отправке сообщения; не
-- прикреплённый за сутки удаляется фоновой задачей.
CREATE TABLE chat_files (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    chat_id     uuid        NOT NULL REFERENCES chats (id),
    message_id  uuid        NULL REFERENCES chat_messages (id),
    uploaded_by uuid        NOT NULL REFERENCES users (id),
    -- Зашифрованное имя файла.
    name        bytea       NOT NULL,
    mime        text        NOT NULL,
    size        bigint      NOT NULL CHECK (size > 0),
    -- Зашифрованное содержимое.
    data        bytea       NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX chat_files_message ON chat_files (message_id) WHERE message_id IS NOT NULL;
CREATE INDEX chat_files_pending ON chat_files (uploaded_by, created_at) WHERE message_id IS NULL;
