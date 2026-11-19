-- Второй фактор для входа из интернета (TOTP, RFC 6238).
-- Секрет хранится зашифрованным (SecretBox, ключ PULT_SECRET_KEY). Пока `totp_enabled_at` пуст,
-- секрет — заготовка: пользователь ещё не подтвердил его кодом из приложения.
ALTER TABLE users
    ADD COLUMN totp_secret     bytea       NULL,
    ADD COLUMN totp_enabled_at timestamptz NULL,
    -- Последний принятый шаг (30 с): тот же код второй раз не принимается.
    ADD COLUMN totp_last_step  bigint      NOT NULL DEFAULT 0,
    ADD CONSTRAINT users_totp_enabled_has_secret CHECK (totp_enabled_at IS NULL OR totp_secret IS NOT NULL);

-- Попытки из интернета считаются отдельно: перебор снаружи не закрывает вход сотрудникам в офисе.
ALTER TABLE login_attempts ADD COLUMN external boolean NOT NULL DEFAULT false;
