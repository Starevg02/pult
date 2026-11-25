-- Вход через провайдера OpenID Connect (Keycloak, ADFS, Entra ID): свой способ входа у сеанса.
ALTER TABLE sessions DROP CONSTRAINT sessions_auth_method_check;
ALTER TABLE sessions ADD CONSTRAINT sessions_auth_method_check
    CHECK (auth_method IN ('ldap', 'ldap+totp', 'kerberos', 'oidc'));

-- Привязка входа через провайдера: издатель и `sub` → пользователь. Логин в ID-токене провайдер может дать
-- сменить, `sub` — постоянный: переименовавшись в чужой логин, под чужой учёткой не войти.
CREATE TABLE oidc_identities (
    issuer        text        NOT NULL,
    subject       text        NOT NULL,
    user_id       uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (issuer, subject),
    UNIQUE (issuer, user_id)
);
