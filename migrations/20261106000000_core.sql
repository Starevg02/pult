-- Ядро — каталог AD, сотрудники, пользователи, права, сеансы, аудит, уведомления, почта.
-- План, раздел 5. Первичные ключи — UUID v7; время — timestamptz (UTC).

-- UUID v7 (RFC 9562): 48 бит времени в мс + случайная часть. В PostgreSQL 17 встроенной функции нет.
CREATE FUNCTION uuid_v7() RETURNS uuid
LANGUAGE sql VOLATILE PARALLEL SAFE AS $$
    SELECT encode(
        set_bit(set_bit(
            overlay(uuid_send(gen_random_uuid())
                    PLACING substring(int8send(floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint) FROM 3)
                    FROM 1 FOR 6),
            52, 1), 53, 1),
        'hex')::uuid
$$;

-- updated_at при каждом изменении строки.
CREATE FUNCTION touch_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END
$$;

-- ---------------------------------------------------------------- Каталог AD (зеркало, только чтение)

CREATE TABLE ad_domains (
    id                 uuid        PRIMARY KEY DEFAULT uuid_v7(),
    dns_name           text        NOT NULL UNIQUE,
    netbios_name       text        NULL,
    base_dn            text        NOT NULL,
    controllers        text[]      NOT NULL CHECK (cardinality(controllers) > 0),
    tls_mode           text        NOT NULL DEFAULT 'ldaps' CHECK (tls_mode IN ('ldaps', 'starttls')),
    bind_dn            text        NOT NULL,
    -- Пароль служебной учётки, зашифрован ключом PULT_SECRET_KEY (AES-256-GCM).
    bind_secret        bytea       NOT NULL,
    -- Сертификат CA домена (PEM), если его нет в системном хранилище.
    ca_cert_pem        text        NULL,
    user_filter        text        NOT NULL DEFAULT '(&(objectCategory=person)(objectClass=user))',
    timeout_secs       integer     NOT NULL DEFAULT 5 CHECK (timeout_secs BETWEEN 1 AND 60),
    -- Состояние инкрементальной синхронизации: {"controller": "...", "usn": 123}.
    sync_state         jsonb       NOT NULL DEFAULT '{}',
    last_full_sync_at  timestamptz NULL,
    last_sync_at       timestamptz NULL,
    enabled            boolean     NOT NULL DEFAULT true,
    version            integer     NOT NULL DEFAULT 1,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER ad_domains_touch BEFORE UPDATE ON ad_domains FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

CREATE TABLE ad_accounts (
    id                   uuid        PRIMARY KEY DEFAULT uuid_v7(),
    domain_id            uuid        NOT NULL REFERENCES ad_domains (id),
    object_guid          uuid        NOT NULL UNIQUE,
    sam_account_name     text        NOT NULL,
    upn                  text        NULL,
    display_name         text        NULL,
    given_name           text        NULL,
    sn                   text        NULL,
    middle_name          text        NULL,
    mail                 text        NULL,
    title                text        NULL,
    department           text        NULL,
    telephone_number     text        NULL,
    mobile               text        NULL,
    dn                   text        NOT NULL,
    user_account_control integer     NOT NULL DEFAULT 0,
    is_enabled           boolean     NOT NULL,
    -- Учётки нет в последней полной синхронизации (удалена или вне фильтра).
    is_missing           boolean     NOT NULL DEFAULT false,
    usn_changed          bigint      NOT NULL DEFAULT 0,
    when_changed         text        NULL,
    synced_at            timestamptz NOT NULL DEFAULT now(),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ad_accounts_sam ON ad_accounts (domain_id, lower(sam_account_name));
CREATE INDEX ad_accounts_upn ON ad_accounts (lower(upn));
CREATE INDEX ad_accounts_mail ON ad_accounts (lower(mail));
CREATE TRIGGER ad_accounts_touch BEFORE UPDATE ON ad_accounts FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

CREATE TABLE ad_sync_runs (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    domain_id   uuid        NOT NULL REFERENCES ad_domains (id),
    kind        text        NOT NULL CHECK (kind IN ('full', 'incremental', 'single')),
    controller  text        NULL,
    status      text        NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'ok', 'error')),
    added       integer     NOT NULL DEFAULT 0,
    updated     integer     NOT NULL DEFAULT 0,
    disabled    integer     NOT NULL DEFAULT 0,
    error       text        NULL,
    started_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz NULL
);
CREATE INDEX ad_sync_runs_recent ON ad_sync_runs (domain_id, started_at DESC);

-- ---------------------------------------------------------------- Площадки, подразделения, должности

CREATE TABLE sites (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name       text        NOT NULL,
    address    text        NULL,
    timezone   text        NOT NULL DEFAULT 'Europe/Moscow',
    is_active  boolean     NOT NULL DEFAULT true,
    version    integer     NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX sites_name ON sites (lower(name));
CREATE TRIGGER sites_touch BEFORE UPDATE ON sites FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

CREATE TABLE departments (
    id                  uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name                text        NOT NULL,
    parent_id           uuid        NULL REFERENCES departments (id),
    is_archived         boolean     NOT NULL DEFAULT false,
    legacy_phonebook_id integer     NULL UNIQUE,
    version             integer     NOT NULL DEFAULT 1,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER departments_touch BEFORE UPDATE ON departments FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

CREATE TABLE positions (
    id                  uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name                text        NOT NULL,
    is_archived         boolean     NOT NULL DEFAULT false,
    legacy_phonebook_id integer     NULL UNIQUE,
    version             integer     NOT NULL DEFAULT 1,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER positions_touch BEFORE UPDATE ON positions FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

-- ---------------------------------------------------------------- Сотрудники (справочник)

CREATE TABLE persons (
    id                  uuid        PRIMARY KEY DEFAULT uuid_v7(),
    last_name           text        NOT NULL DEFAULT '',
    first_name          text        NOT NULL DEFAULT '',
    middle_name         text        NULL,
    department_id       uuid        NULL REFERENCES departments (id),
    position_id         uuid        NULL REFERENCES positions (id),
    work_phone          text        NULL,
    -- Добавочный (3–4 цифры) — для звонка по клику.
    internal_ext        text        NULL,
    mobile_phone        text        NULL,
    email               text        NULL,
    site_id             uuid        NULL REFERENCES sites (id),
    room                text        NULL,
    ad_account_id       uuid        NULL UNIQUE REFERENCES ad_accounts (id),
    is_archived         boolean     NOT NULL DEFAULT false,
    archived_at         timestamptz NULL,
    -- Кто отправил в архив: manual — редактор, ad_disabled — синхронизация (вернёт сама), legacy — импорт.
    archived_reason     text        NULL CHECK (archived_reason IN ('manual', 'ad_disabled', 'legacy')),
    source              text        NOT NULL CHECK (source IN ('ad', 'manual', 'phonebook_import')),
    legacy_phonebook_id integer     NULL UNIQUE,
    -- Нормализованный текст для поиска (нижний регистр, ё → е) — заполняет приложение.
    search_text         text        NOT NULL DEFAULT '',
    -- Все цифры телефонов через пробел — поиск по номеру без учёта форматирования.
    phone_digits        text        NOT NULL DEFAULT '',
    version             integer     NOT NULL DEFAULT 1,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX persons_search ON persons USING gin (search_text gin_trgm_ops);
CREATE INDEX persons_phones ON persons USING gin (phone_digits gin_trgm_ops);
CREATE INDEX persons_department ON persons (department_id) WHERE NOT is_archived;
CREATE INDEX persons_name ON persons (lower(last_name), lower(first_name));
CREATE TRIGGER persons_touch BEFORE UPDATE ON persons FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

-- Откуда берётся поле справочника. Нет строки — значение по умолчанию из кода.
CREATE TABLE person_field_sources (
    field      text        PRIMARY KEY,
    source     text        NOT NULL CHECK (source IN ('ad', 'portal')),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Спорные пары «учётка AD — сотрудник» на подтверждение администратором.
CREATE TABLE person_link_candidates (
    id            uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ad_account_id uuid        NOT NULL REFERENCES ad_accounts (id),
    person_id     uuid        NOT NULL REFERENCES persons (id),
    reason        text        NOT NULL,
    status        text        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'rejected')),
    decided_by    uuid        NULL,
    decided_at    timestamptz NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (ad_account_id, person_id)
);

CREATE TABLE phonebook_import_runs (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    source      text        NOT NULL,
    dry_run     boolean     NOT NULL,
    counts      jsonb       NOT NULL DEFAULT '{}',
    errors      jsonb       NOT NULL DEFAULT '[]',
    started_by  uuid        NULL,
    started_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz NULL
);

-- ---------------------------------------------------------------- Пользователи портала и права

CREATE TABLE users (
    id            uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ad_account_id uuid        NOT NULL UNIQUE REFERENCES ad_accounts (id),
    person_id     uuid        NULL REFERENCES persons (id),
    is_active     boolean     NOT NULL DEFAULT true,
    locale        text        NOT NULL DEFAULT 'ru',
    settings      jsonb       NOT NULL DEFAULT '{}',
    last_login_at timestamptz NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER users_touch BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

ALTER TABLE person_link_candidates ADD FOREIGN KEY (decided_by) REFERENCES users (id);
ALTER TABLE phonebook_import_runs ADD FOREIGN KEY (started_by) REFERENCES users (id);
ALTER TABLE settings ADD FOREIGN KEY (updated_by) REFERENCES users (id);

CREATE TABLE person_favorites (
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    person_id  uuid        NOT NULL REFERENCES persons (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, person_id)
);

CREATE TABLE site_engineers (
    site_id    uuid        NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    user_id    uuid        NOT NULL REFERENCES users (id),
    line       text        NOT NULL CHECK (line IN ('l1', 'l2')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (site_id, user_id, line)
);

CREATE TABLE roles (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    code       text        NOT NULL UNIQUE,
    name       text        NOT NULL,
    is_system  boolean     NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER roles_touch BEFORE UPDATE ON roles FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

CREATE TABLE role_permissions (
    role_id    uuid NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    permission text NOT NULL,
    PRIMARY KEY (role_id, permission)
);

CREATE TABLE user_roles (
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role_id    uuid        NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    -- Ограничение площадкой; NULL — на все площадки.
    site_id    uuid        NULL REFERENCES sites (id) ON DELETE CASCADE,
    granted_by uuid        NULL REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (user_id, role_id, site_id)
);

-- Системные роли. Права по умолчанию задаёт приложение при старте (только для новых ролей).
INSERT INTO roles (code, name, is_system) VALUES
    ('employee', 'Сотрудник', true),
    ('engineer', 'Инженер', true),
    ('phonebook_editor', 'Редактор справочника', true),
    ('superadmin', 'Суперадминистратор', true),
    ('auditor', 'Аудитор', true);

-- ---------------------------------------------------------------- Сеансы и попытки входа

CREATE TABLE sessions (
    -- SHA-256 токена из cookie; сам токен в БД не хранится.
    id                  bytea       PRIMARY KEY CHECK (length(id) = 32),
    user_id             uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    auth_method         text        NOT NULL CHECK (auth_method IN ('ldap', 'ldap+totp')),
    -- Токен CSRF (double-submit): тот же, что в cookie `pult_csrf`; привязан к сеансу.
    csrf_token          text        NOT NULL,
    ip                  inet        NULL,
    user_agent          text        NULL,
    created_at          timestamptz NOT NULL DEFAULT now(),
    last_seen_at        timestamptz NOT NULL DEFAULT now(),
    idle_expires_at     timestamptz NOT NULL,
    absolute_expires_at timestamptz NOT NULL
);
CREATE INDEX sessions_user ON sessions (user_id);
CREATE INDEX sessions_expiry ON sessions (absolute_expires_at);

CREATE TABLE login_attempts (
    id     bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    login  text        NOT NULL,
    ip     inet        NULL,
    result text        NOT NULL,
    ts     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX login_attempts_login ON login_attempts (lower(login), ts DESC);
CREATE INDEX login_attempts_ip ON login_attempts (ip, ts DESC);

-- ---------------------------------------------------------------- Аудит

CREATE TABLE audit_log (
    -- Номер в цепочке; присваивает триггер под блокировкой (не последовательность: её значения
    -- выдаются до блокировки, и параллельные вставки перепутали бы порядок).
    id              bigint      PRIMARY KEY,
    ts              timestamptz NOT NULL DEFAULT now(),
    actor_id        uuid        NULL REFERENCES users (id),
    on_behalf_of_id uuid        NULL REFERENCES users (id),
    action          text        NOT NULL,
    object_type     text        NULL,
    object_id       text        NULL,
    details         jsonb       NOT NULL DEFAULT '{}',
    ip              inet        NULL,
    user_agent      text        NULL,
    source          text        NOT NULL CHECK (source IN ('portal', 'api', 'email', 'agent', 'job', 'legacy_phonebook', 'cli')),
    prev_hash       bytea       NOT NULL,
    hash            bytea       NOT NULL
);
CREATE INDEX audit_log_ts ON audit_log (ts DESC);
CREATE INDEX audit_log_actor ON audit_log (actor_id, ts DESC);
CREATE INDEX audit_log_object ON audit_log (object_type, object_id, ts DESC);

-- Содержимое записи для хэша: поля в фиксированном порядке, время — в микросекундах UTC,
-- jsonb — в каноническом виде (ключи упорядочены самим jsonb).
CREATE FUNCTION audit_payload(r audit_log) RETURNS bytea
LANGUAGE sql IMMUTABLE AS $$
    SELECT convert_to(concat_ws(E'\x1f',
        r.id::text,
        (extract(epoch FROM r.ts) * 1000000)::bigint::text,
        coalesce(r.actor_id::text, ''),
        coalesce(r.on_behalf_of_id::text, ''),
        r.action,
        coalesce(r.object_type, ''),
        coalesce(r.object_id, ''),
        r.details::text,
        coalesce(host(r.ip), ''),
        coalesce(r.user_agent, ''),
        r.source), 'UTF8')
$$;

-- Цепочка: hash = SHA-256(prev_hash || содержимое). Вставки идут по одной (блокировка), поэтому
-- порядок id совпадает с порядком цепочки.
CREATE FUNCTION audit_chain() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    last_id bigint;
    last bytea;
BEGIN
    -- Функция VOLATILE: запрос после блокировки видит записи, закоммиченные во время ожидания.
    PERFORM pg_advisory_xact_lock(7231001);
    SELECT id, hash INTO last_id, last FROM audit_log ORDER BY id DESC LIMIT 1;
    NEW.id := coalesce(last_id, 0) + 1;
    NEW.ts := clock_timestamp();
    NEW.prev_hash := coalesce(last, '\x'::bytea);
    NEW.hash := sha256(NEW.prev_hash || audit_payload(NEW));
    RETURN NEW;
END
$$;
CREATE TRIGGER audit_log_chain BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION audit_chain();

-- Только вставка: изменить или удалить запись нельзя.
CREATE FUNCTION audit_readonly() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_log: изменение и удаление записей запрещены';
END
$$;
CREATE TRIGGER audit_log_no_update BEFORE UPDATE OR DELETE ON audit_log FOR EACH ROW EXECUTE FUNCTION audit_readonly();
CREATE TRIGGER audit_log_no_truncate BEFORE TRUNCATE ON audit_log FOR EACH STATEMENT EXECUTE FUNCTION audit_readonly();

-- Первая запись с нарушенной цепочкой (NULL — цепочка цела). Проверяет записи с id > after_id.
CREATE FUNCTION audit_verify(after_id bigint DEFAULT 0) RETURNS bigint
LANGUAGE plpgsql STABLE AS $$
DECLARE
    r audit_log;
    prev bytea;
BEGIN
    SELECT hash INTO prev FROM audit_log WHERE id <= after_id ORDER BY id DESC LIMIT 1;
    prev := coalesce(prev, '\x'::bytea);
    FOR r IN SELECT * FROM audit_log WHERE id > after_id ORDER BY id LOOP
        IF r.prev_hash <> prev OR r.hash <> sha256(r.prev_hash || audit_payload(r)) THEN
            RETURN r.id;
        END IF;
        prev := r.hash;
    END LOOP;
    RETURN NULL;
END
$$;

-- ---------------------------------------------------------------- Уведомления и почта

CREATE TABLE notifications (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind       text        NOT NULL,
    payload    jsonb       NOT NULL DEFAULT '{}',
    read_at    timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX notifications_unread ON notifications (user_id, created_at DESC) WHERE read_at IS NULL;
CREATE INDEX notifications_user ON notifications (user_id, created_at DESC);

CREATE TABLE notification_prefs (
    user_id uuid    NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind    text    NOT NULL,
    email   boolean NOT NULL DEFAULT true,
    PRIMARY KEY (user_id, kind)
);

CREATE TABLE outbox_emails (
    id              uuid        PRIMARY KEY DEFAULT uuid_v7(),
    to_addrs        text[]      NOT NULL CHECK (cardinality(to_addrs) > 0),
    subject         text        NOT NULL,
    body_text       text        NOT NULL,
    body_html       text        NULL,
    headers         jsonb       NOT NULL DEFAULT '{}',
    status          text        NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'sending', 'sent', 'failed')),
    attempts        integer     NOT NULL DEFAULT 0,
    last_error      text        NULL,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    sent_at         timestamptz NULL,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX outbox_emails_due ON outbox_emails (next_attempt_at) WHERE status = 'queued';
