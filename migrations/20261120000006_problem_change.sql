-- Проблемы и изменения (по образцу GLPI): заявки другого типа, создают только ИТ-специалисты.
-- Общее с заявками (исполнитель, лента, чек-лист, оборудование, наблюдатели) — из tickets,
-- своё — в отдельных таблицах.
ALTER TABLE tickets DROP CONSTRAINT tickets_kind_check;
ALTER TABLE tickets ADD CONSTRAINT tickets_kind_check CHECK (kind IN ('incident', 'request', 'problem', 'change'));

-- Инцидент — следствие проблемы. У инцидента не больше одной проблемы.
ALTER TABLE tickets ADD COLUMN problem_id uuid NULL REFERENCES tickets (id) ON DELETE SET NULL;
CREATE INDEX tickets_problem ON tickets (problem_id) WHERE problem_id IS NOT NULL;
-- Списки «Проблемы» и «Изменения».
CREATE INDEX tickets_itil ON tickets (kind, status, updated_at DESC) WHERE kind IN ('problem', 'change');

CREATE TABLE ticket_problems (
    ticket_id   uuid        PRIMARY KEY REFERENCES tickets (id) ON DELETE CASCADE,
    -- Причина и обходное решение; «известная ошибка» — причина найдена, обходное решение есть.
    cause       text        NOT NULL DEFAULT '' CHECK (length(cause) <= 20000),
    workaround  text        NOT NULL DEFAULT '' CHECK (length(workaround) <= 20000),
    known_error boolean     NOT NULL DEFAULT false,
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ticket_problems_known ON ticket_problems (ticket_id) WHERE known_error;
CREATE TRIGGER ticket_problems_touch BEFORE UPDATE ON ticket_problems FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

CREATE TABLE ticket_changes (
    ticket_id     uuid        PRIMARY KEY REFERENCES tickets (id) ON DELETE CASCADE,
    risk          text        NOT NULL DEFAULT 'low' CHECK (risk IN ('low', 'medium', 'high')),
    -- План работ, проверка результата и откат.
    plan          text        NOT NULL DEFAULT '' CHECK (length(plan) <= 20000),
    rollback      text        NOT NULL DEFAULT '' CHECK (length(rollback) <= 20000),
    planned_start timestamptz NULL,
    planned_end   timestamptz NULL,
    -- draft — готовится; pending — на согласовании; approved — можно выполнять; rejected — вернули на доработку.
    approval      text        NOT NULL DEFAULT 'draft' CHECK (approval IN ('draft', 'pending', 'approved', 'rejected')),
    -- Номер круга согласования: правка после решения начинает новый круг.
    round         integer     NOT NULL DEFAULT 0,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK (planned_end IS NULL OR planned_start IS NULL OR planned_end >= planned_start)
);
CREATE TRIGGER ticket_changes_touch BEFORE UPDATE ON ticket_changes FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

-- Кто согласует изменение: по категории (пусто — любая) и начиная с уровня риска.
-- Подходят все правила, чья категория и риск совпали; согласующие объединяются.
CREATE TABLE change_approval_rules (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    category_id uuid        NULL REFERENCES ticket_categories (id) ON DELETE CASCADE,
    min_risk    text        NOT NULL DEFAULT 'low' CHECK (min_risk IN ('low', 'medium', 'high')),
    approver_id uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_by  uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX change_approval_rules_uniq
    ON change_approval_rules (coalesce(category_id, '00000000-0000-0000-0000-000000000000'::uuid), min_risk, approver_id);
CREATE INDEX change_approval_rules_approver ON change_approval_rules (approver_id);

CREATE TABLE change_approvals (
    id          uuid        PRIMARY KEY DEFAULT uuid_v7(),
    ticket_id   uuid        NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    round       integer     NOT NULL,
    approver_id uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- skipped — круг завершён без этого решения (кто-то отклонил, изменение отозвали на правку).
    status      text        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected', 'skipped')),
    comment     text        NULL CHECK (length(comment) <= 4000),
    decided_at  timestamptz NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (ticket_id, round, approver_id)
);
CREATE INDEX change_approvals_pending ON change_approvals (approver_id, created_at DESC) WHERE status = 'pending';
CREATE INDEX change_approvals_approver ON change_approvals (approver_id, decided_at DESC) WHERE status <> 'pending';
