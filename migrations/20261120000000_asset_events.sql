-- История оборудования: выдача, возврат, передача, смена состояния и правки карточки.
-- Имена сотрудников запоминаются: карточку человека могут удалить, история останется.
CREATE TABLE asset_events (
    id             uuid        PRIMARY KEY DEFAULT uuid_v7(),
    asset_id       uuid        NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    kind           text        NOT NULL CHECK (kind IN ('created', 'issued', 'returned', 'transferred', 'updated')),
    from_person_id uuid        NULL REFERENCES persons (id) ON DELETE SET NULL,
    from_name      text        NULL,
    to_person_id   uuid        NULL REFERENCES persons (id) ON DELETE SET NULL,
    to_name        text        NULL,
    -- Изменённые поля карточки: {"поле": [было, стало]}.
    changes        jsonb       NOT NULL DEFAULT '{}',
    comment        text        NULL CHECK (length(comment) <= 1000),
    actor_id       uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX asset_events_asset ON asset_events (asset_id, created_at DESC);
CREATE INDEX asset_events_from ON asset_events (from_person_id) WHERE from_person_id IS NOT NULL;
CREATE INDEX asset_events_to ON asset_events (to_person_id) WHERE to_person_id IS NOT NULL;

-- Уже заведённое оборудование: строка «добавлено» с текущим ответственным, чтобы история не начиналась с пустоты.
INSERT INTO asset_events (asset_id, kind, to_person_id, to_name, created_at)
SELECT a.id, 'created', a.responsible_person_id,
       nullif(trim(concat_ws(' ', p.last_name, p.first_name, p.middle_name)), ''), a.created_at
  FROM assets a LEFT JOIN persons p ON p.id = a.responsible_person_id;
