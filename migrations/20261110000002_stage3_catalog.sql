-- Шаблоны заявок и ответов, регламентные заявки, база знаний, каталог оборудования
-- и связанные объекты, связи заявок, токены API и вебхуки.

CREATE TABLE ticket_templates (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name        text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    category_id uuid        NULL REFERENCES ticket_categories (id) ON DELETE SET NULL,
    group_id    uuid        NULL REFERENCES engineer_groups (id) ON DELETE SET NULL,
    title       text        NOT NULL CHECK (length(title) BETWEEN 1 AND 300),
    description text        NOT NULL DEFAULT '',
    -- [{title, assignee_id, is_required}]
    checklist   jsonb       NOT NULL DEFAULT '[]',
    audience    text        NOT NULL DEFAULT 'engineers' CHECK (audience IN ('all', 'engineers')),
    is_active   boolean     NOT NULL DEFAULT true,
    sort        integer     NOT NULL DEFAULT 100,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Стартовые шаблоны. Исполнителей пунктов назначает администратор.
INSERT INTO ticket_templates (name, title, description, checklist, sort) VALUES
('Новый сотрудник', 'Новый сотрудник: ФИО, должность, дата выхода',
 'ФИО:\nПодразделение и должность:\nДата выхода:\nРуководитель:\nНужные программы и доступы:',
 '[{"title": "Учётная запись в AD", "assignee_id": null, "is_required": true},
   {"title": "Почтовый ящик", "assignee_id": null, "is_required": true},
   {"title": "Группы и доступы к папкам", "assignee_id": null, "is_required": true},
   {"title": "Рабочее место: ПК, монитор, телефон", "assignee_id": null, "is_required": true},
   {"title": "Программы (1С и прочие)", "assignee_id": null, "is_required": true},
   {"title": "Запись в справочник, внутренний номер", "assignee_id": null, "is_required": false}]', 10),
('Увольнение сотрудника', 'Увольнение: ФИО, последний рабочий день',
 'ФИО:\nПоследний рабочий день:\nКому передать почту и файлы:',
 '[{"title": "Отключить учётную запись в AD", "assignee_id": null, "is_required": true},
   {"title": "Почта: переадресация или архив", "assignee_id": null, "is_required": true},
   {"title": "Снять доступы и группы", "assignee_id": null, "is_required": true},
   {"title": "Забрать оборудование", "assignee_id": null, "is_required": true},
   {"title": "Отметить в справочнике", "assignee_id": null, "is_required": false}]', 20),
('Перевод в другой отдел', 'Перевод: ФИО, новый отдел и должность',
 'ФИО:\nНовое подразделение и должность:\nДата перевода:',
 '[{"title": "Изменить подразделение и должность в AD", "assignee_id": null, "is_required": true},
   {"title": "Пересмотреть группы и доступы", "assignee_id": null, "is_required": true},
   {"title": "Перенести рабочее место", "assignee_id": null, "is_required": false},
   {"title": "Обновить справочник", "assignee_id": null, "is_required": false}]', 30);

CREATE TABLE recurring_tickets (
    id             uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name           text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    template_id    uuid        NOT NULL REFERENCES ticket_templates (id) ON DELETE CASCADE,
    -- cron, местное время площадки.
    schedule       text        NOT NULL,
    site_id        uuid        NULL REFERENCES sites (id) ON DELETE CASCADE,
    requester_id   uuid        NULL REFERENCES persons (id) ON DELETE SET NULL,
    is_active      boolean     NOT NULL DEFAULT true,
    next_run_at    timestamptz NULL,
    last_run_at    timestamptz NULL,
    last_ticket_id uuid        NULL REFERENCES tickets (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX recurring_due ON recurring_tickets (next_run_at) WHERE is_active;
ALTER TABLE tickets ADD COLUMN recurring_id uuid NULL REFERENCES recurring_tickets (id) ON DELETE SET NULL,
                    ADD COLUMN template_id uuid NULL REFERENCES ticket_templates (id) ON DELETE SET NULL;

-- Шаблоны ответов: owner_id NULL — общий.
CREATE TABLE response_templates (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    title      text        NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    body       text        NOT NULL,
    owner_id   uuid        NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX response_templates_owner ON response_templates (owner_id);
INSERT INTO response_templates (title, body) VALUES
('Приветствие', 'Здравствуйте, {{requester.first_name}}! Заявку #{{ticket.number}} взял в работу.'),
('Нужна информация', 'Здравствуйте, {{requester.first_name}}! Чтобы решить заявку, пришлите, пожалуйста, скриншот ошибки и имя компьютера.'),
('Решено', 'Готово. Если что-то не так — нажмите «Нет, не решена», заявка вернётся ко мне. {{engineer.name}}');

CREATE TABLE kb_sections (
    id   uuid    PRIMARY KEY DEFAULT uuid_v7(),
    name text    NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    sort integer NOT NULL DEFAULT 0
);
CREATE TABLE kb_articles (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    title            text        NOT NULL CHECK (length(title) BETWEEN 1 AND 300),
    body             text        NOT NULL DEFAULT '',
    audience         text        NOT NULL DEFAULT 'all' CHECK (audience IN ('all', 'engineers')),
    status           text        NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    section_id       uuid        NULL REFERENCES kb_sections (id) ON DELETE SET NULL,
    author_id        uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    source_ticket_id uuid        NULL REFERENCES tickets (id) ON DELETE SET NULL,
    views            integer     NOT NULL DEFAULT 0,
    search_tsv       tsvector    GENERATED ALWAYS AS (to_tsvector('russian'::regconfig, title || ' ' || body)) STORED,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX kb_articles_search ON kb_articles USING gin (search_tsv);

-- Каталог оборудования: типы с дополнительными полями и сами объекты.
CREATE TABLE asset_types (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name       text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    fields     jsonb       NOT NULL DEFAULT '[]',
    is_active  boolean     NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX asset_types_name ON asset_types (lower(name));
INSERT INTO asset_types (name, fields) VALUES
('Принтер / МФУ', '[{"key": "model", "label": "Модель", "type": "text"}, {"key": "cartridge", "label": "Картридж", "type": "text"}, {"key": "ip", "label": "IP-адрес", "type": "text"}]'),
('Монитор', '[{"key": "model", "label": "Модель", "type": "text"}]'),
('Ноутбук', '[{"key": "model", "label": "Модель", "type": "text"}]'),
('ИБП', '[{"key": "model", "label": "Модель", "type": "text"}, {"key": "battery_date", "label": "Замена батареи", "type": "text"}]'),
('Телефон', '[{"key": "model", "label": "Модель", "type": "text"}, {"key": "ext", "label": "Внутренний номер", "type": "text"}]'),
('Сетевое оборудование', '[{"key": "model", "label": "Модель", "type": "text"}, {"key": "ip", "label": "IP-адрес", "type": "text"}]'),
('Программа / лицензия', '[{"key": "vendor", "label": "Производитель", "type": "text"}, {"key": "expires", "label": "Действует до", "type": "text"}, {"key": "seats", "label": "Число мест", "type": "text"}]'),
('Прочее', '[]');

CREATE TABLE assets (
    id                    uuid        PRIMARY KEY DEFAULT uuid_v7(),
    type_id               uuid        NOT NULL REFERENCES asset_types (id),
    name                  text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    inventory_number      text        NULL,
    serial_number         text        NULL,
    site_id               uuid        NULL REFERENCES sites (id) ON DELETE SET NULL,
    room                  text        NULL,
    responsible_person_id uuid        NULL REFERENCES persons (id) ON DELETE SET NULL,
    status                text        NOT NULL DEFAULT 'in_use' CHECK (status IN ('in_use', 'in_stock', 'repair', 'retired')),
    notes                 text        NULL,
    attributes            jsonb       NOT NULL DEFAULT '{}',
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX assets_inventory ON assets (lower(inventory_number)) WHERE inventory_number IS NOT NULL;
CREATE INDEX assets_type ON assets (type_id);
CREATE INDEX assets_responsible ON assets (responsible_person_id);

-- Связанные объекты заявки. Название запоминается: объект могут удалить, история останется.
CREATE TABLE ticket_objects (
    ticket_id  uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    kind       text        NOT NULL CHECK (kind IN ('computer', 'person', 'ad_account', 'asset')),
    object_id  uuid        NOT NULL,
    label      text        NOT NULL,
    detail     text        NULL,
    added_by   uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (ticket_id, kind, object_id)
);
CREATE INDEX ticket_objects_object ON ticket_objects (kind, object_id);

CREATE TABLE ticket_links (
    ticket_id        uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    linked_ticket_id uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    kind             text        NOT NULL CHECK (kind IN ('related', 'duplicate', 'follow_up')),
    created_by       uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (ticket_id, linked_ticket_id),
    CHECK (ticket_id <> linked_ticket_id)
);
CREATE INDEX ticket_links_linked ON ticket_links (linked_ticket_id);

CREATE TABLE api_tokens (
    id            uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name          text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    owner_user_id uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash    bytea       NOT NULL UNIQUE,
    permissions   text[]      NOT NULL DEFAULT '{}',
    expires_at    timestamptz NULL,
    last_used_at  timestamptz NULL,
    revoked_at    timestamptz NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE webhooks (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name             text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    url              text        NOT NULL,
    events           text[]      NOT NULL DEFAULT '{}',
    -- Секрет подписи, зашифрован ключом сервера.
    secret           bytea       NOT NULL,
    is_active        boolean     NOT NULL DEFAULT true,
    last_status      integer     NULL,
    last_error       text        NULL,
    last_delivery_at timestamptz NULL,
    created_at       timestamptz NOT NULL DEFAULT now()
);
