-- Каталог AD и доступы: операции записи в AD, ресурсы с владельцами,
-- запросы доступа с согласованием владельцем, действия чек-листа через каталог.

-- Служебная учётка записи — отдельно от учётки чтения, с делегированными правами (не Domain Admins).
-- Пароль зашифрован ключом PULT_SECRET_KEY, как bind_secret.
ALTER TABLE ad_domains ADD COLUMN writer_bind_dn text  NULL,
                       ADD COLUMN writer_secret  bytea NULL;

-- Состояние учётки для карточки заявителя: читается из AD точечно, когда карточку открывают.
ALTER TABLE ad_accounts ADD COLUMN lockout_time        timestamptz NULL,
                        ADD COLUMN pwd_last_set        timestamptz NULL,
                        ADD COLUMN password_expires_at timestamptz NULL,
                        ADD COLUMN last_logon          timestamptz NULL,
                        ADD COLUMN state_read_at       timestamptz NULL;

-- Операция записи в AD: создаётся по запросу пользователя, выполняется фоновой задачей.
-- Ключ идемпотентности: повтор запроса или повторная доставка задачи не выполняет операцию дважды.
CREATE TABLE directory_operations (
    id                uuid        PRIMARY KEY DEFAULT uuid_v7(),
    idempotency_key   text        NOT NULL UNIQUE CHECK (length(idempotency_key) BETWEEN 8 AND 200),
    kind              text        NOT NULL CHECK (kind IN ('group.add_member', 'user.unlock', 'user.enable',
                                                           'user.disable', 'user.reset_password')),
    account_id        uuid        NOT NULL REFERENCES ad_accounts (id),
    target_dn         text        NOT NULL,
    group_dn          text        NULL CHECK ((kind = 'group.add_member') = (group_dn IS NOT NULL)),
    ticket_id         uuid        NULL REFERENCES tickets (id) ON DELETE SET NULL,
    checklist_item_id uuid        NULL REFERENCES ticket_checklist_items (id) ON DELETE SET NULL,
    -- Основание: комментарий и (по настройке обязательно) заявка.
    basis             text        NOT NULL CHECK (length(basis) BETWEEN 1 AND 2000),
    requested_by      uuid        NOT NULL REFERENCES users (id),
    status            text        NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'failed')),
    -- Состояние учётки до и после (повторное чтение); `changed = false` — уже было так.
    before            jsonb       NULL,
    after             jsonb       NULL,
    changed           boolean     NULL,
    -- Повторное чтение подтвердило нужное состояние.
    verified          boolean     NULL,
    error             text        NULL,
    -- Временный пароль (зашифрован) — до первого показа запросившему, потом стирается.
    secret            bytea       NULL,
    secret_shown_at   timestamptz NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    finished_at       timestamptz NULL,
    -- Итог записан в заявку (комментарий, запрос доступа, пункт чек-листа) — ровно один раз.
    reported_at       timestamptz NULL
);
CREATE INDEX directory_operations_ticket ON directory_operations (ticket_id, created_at);
CREATE INDEX directory_operations_account ON directory_operations (account_id, created_at DESC);
CREATE INDEX directory_operations_item ON directory_operations (checklist_item_id) WHERE checklist_item_id IS NOT NULL;
CREATE INDEX directory_operations_secret ON directory_operations (finished_at) WHERE secret IS NOT NULL;

-- Ресурсы для запросов доступа: у каждого ровно один владелец-сотрудник.
CREATE TABLE access_resources (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    kind        text        NOT NULL CHECK (kind IN ('ad_group', 'share', 'application', 'other')),
    name        text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description text        NOT NULL DEFAULT '',
    -- Для `ad_group` — группа, в которую добавляет «Выполнить»; для общей папки — её группа, если есть.
    group_dn    text        NULL CHECK (kind <> 'ad_group' OR group_dn IS NOT NULL),
    owner_id    uuid        NOT NULL REFERENCES persons (id),
    is_active   boolean     NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX access_resources_name ON access_resources (lower(name));
CREATE TRIGGER access_resources_touch BEFORE UPDATE ON access_resources FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

-- Запрос доступа: одна заявка — один ресурс. Согласует владелец ресурса.
CREATE TABLE access_requests (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id        uuid        NOT NULL UNIQUE REFERENCES tickets (id) ON DELETE CASCADE,
    resource_id      uuid        NOT NULL REFERENCES access_resources (id),
    -- Кому нужен доступ (заявитель).
    person_id        uuid        NULL REFERENCES persons (id) ON DELETE SET NULL,
    status           text        NOT NULL DEFAULT 'new'
                                 CHECK (status IN ('new', 'pending', 'approved', 'rejected', 'done')),
    -- Кто согласует: владелец ресурса; смена владельца переносит ожидающие согласования.
    approver_id      uuid        NULL REFERENCES persons (id),
    sent_by          uuid        NULL REFERENCES users (id),
    sent_at          timestamptz NULL,
    decided_by       uuid        NULL REFERENCES users (id),
    decided_at       timestamptz NULL,
    decision_comment text        NULL,
    operation_id     uuid        NULL REFERENCES directory_operations (id),
    granted_at       timestamptz NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX access_requests_approver ON access_requests (approver_id) WHERE status = 'pending';
CREATE INDEX access_requests_person ON access_requests (person_id, created_at DESC);
CREATE INDEX access_requests_resource ON access_requests (resource_id) WHERE status = 'pending';
CREATE INDEX access_requests_operation ON access_requests (operation_id) WHERE operation_id IS NOT NULL;
CREATE INDEX access_requests_decided_by ON access_requests (decided_by) WHERE decided_by IS NOT NULL;
CREATE TRIGGER access_requests_touch BEFORE UPDATE ON access_requests FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

ALTER TABLE tickets ADD COLUMN resource_id uuid NULL REFERENCES access_resources (id);

-- Пункт чек-листа, который выполняется через каталог AD.
ALTER TABLE ticket_checklist_items
    ADD COLUMN action text NULL CHECK (action IN ('directory.enable', 'directory.disable', 'directory.add_member'));

-- Категория «Запрос доступа» — служебная: форма заявки просит ресурс.
UPDATE ticket_categories SET code = 'access' WHERE code IS NULL AND name = 'Запрос доступа'
   AND NOT EXISTS (SELECT 1 FROM ticket_categories WHERE code = 'access');
INSERT INTO ticket_categories (name, kind, code, sort)
SELECT 'Запрос доступа', 'request', 'access', 80
 WHERE NOT EXISTS (SELECT 1 FROM ticket_categories WHERE code = 'access');

-- «Новый сотрудник»: учётка и группы — кнопками каталога; «Увольнение»: отключение учётки.
UPDATE ticket_templates t SET checklist = (
    SELECT coalesce(jsonb_agg(CASE i->>'title'
                                WHEN 'Учётная запись в AD' THEN i || '{"action": "directory.enable"}'
                                WHEN 'Группы и доступы к папкам' THEN i || '{"action": "directory.add_member"}'
                                WHEN 'Отключить учётную запись в AD' THEN i || '{"action": "directory.disable"}'
                                ELSE i END ORDER BY n), '[]'::jsonb)
      FROM jsonb_array_elements(t.checklist) WITH ORDINALITY AS x(i, n))
 WHERE t.name IN ('Новый сотрудник', 'Увольнение сотрудника');
