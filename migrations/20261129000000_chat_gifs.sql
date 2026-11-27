-- «Мои GIF»: личная подборка анимаций сотрудника. Содержимое зашифровано ключом портала; один и тот же
-- GIF в подборке одного сотрудника — один раз (по SHA-256 содержимого).
CREATE TABLE chat_gifs (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    sha256     bytea       NOT NULL,
    size       bigint      NOT NULL CHECK (size > 0),
    data       bytea       NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, sha256)
);
CREATE INDEX chat_gifs_user ON chat_gifs (user_id, created_at DESC);
