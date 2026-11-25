-- Уведомления на устройства (Web Push): подписки браузеров и отметка о рассылке.

CREATE TABLE push_subscriptions (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    endpoint   text        NOT NULL UNIQUE CHECK (length(endpoint) <= 2000),
    p256dh     bytea       NOT NULL CHECK (length(p256dh) = 65),
    auth       bytea       NOT NULL CHECK (length(auth) = 16),
    user_agent text        NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_ok_at timestamptz NULL,
    failures   integer     NOT NULL DEFAULT 0,
    last_error text        NULL
);
CREATE INDEX push_subscriptions_user ON push_subscriptions (user_id);

-- Уведомление забрано рассылкой (отправлено или пропущено). Прежние уведомления не рассылаются.
ALTER TABLE notifications ADD COLUMN pushed_at timestamptz NULL;
UPDATE notifications SET pushed_at = created_at;
CREATE INDEX notifications_unpushed ON notifications (created_at) WHERE pushed_at IS NULL;
