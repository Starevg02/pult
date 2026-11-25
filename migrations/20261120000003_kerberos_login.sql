-- Вход через домен без пароля (Kerberos): свой способ входа у сеанса.
ALTER TABLE sessions DROP CONSTRAINT sessions_auth_method_check;
ALTER TABLE sessions ADD CONSTRAINT sessions_auth_method_check
    CHECK (auth_method IN ('ldap', 'ldap+totp', 'kerberos'));
