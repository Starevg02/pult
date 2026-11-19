-- Этап 3, карточка заявки: наблюдатели, трудозатраты, код закрытия.

-- Наблюдатель — сотрудник справочника: видит заявку и публичные ответы, получает письма.
CREATE TABLE ticket_watchers (
    ticket_id  uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    person_id  uuid        NOT NULL REFERENCES persons (id) ON DELETE CASCADE,
    added_by   uuid        NULL REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (ticket_id, person_id)
);
CREATE INDEX ticket_watchers_person ON ticket_watchers (person_id);

-- Затраченное время инженера: к ответу, заметке, решению или отдельно.
CREATE TABLE ticket_worklogs (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id  uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    user_id    uuid        NULL REFERENCES users (id),
    minutes    integer     NOT NULL CHECK (minutes BETWEEN 1 AND 2400),
    note       text        NULL,
    comment_id uuid        NULL REFERENCES ticket_comments (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ticket_worklogs_ticket ON ticket_worklogs (ticket_id);
CREATE INDEX ticket_worklogs_user ON ticket_worklogs (user_id, created_at);

-- Код закрытия из списка в настройках (решено, обходное решение, дубль…).
ALTER TABLE tickets ADD COLUMN closure_code text NULL;

-- Задачи внутри заявки (чек-лист): у пункта свой исполнитель; обязательные не дают решить заявку.
CREATE TABLE ticket_checklist_items (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id   uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    position    integer     NOT NULL DEFAULT 0,
    title       text        NOT NULL CHECK (length(title) BETWEEN 1 AND 300),
    assignee_id uuid        NULL REFERENCES users (id),
    is_required boolean     NOT NULL DEFAULT true,
    done_at     timestamptz NULL,
    done_by     uuid        NULL REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ticket_checklist_ticket ON ticket_checklist_items (ticket_id, position);
CREATE INDEX ticket_checklist_assignee ON ticket_checklist_items (assignee_id) WHERE done_at IS NULL;
