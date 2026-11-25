-- Инвентарь из внешних систем (NetBox): сетевое оборудование, серверы, виртуальные машины.
-- Как и ПК с агентом, в каталог попадает только после проверки инженером; принятое потом
-- не обновляется само — карточку меняют вручную.
CREATE TABLE external_inventory (
    id            uuid        PRIMARY KEY DEFAULT uuid_v7(),
    source        text        NOT NULL CHECK (source IN ('netbox')),
    external_id   text        NOT NULL CHECK (length(external_id) BETWEEN 1 AND 100),
    -- `device` — устройство, `vm` — виртуальная машина.
    kind          text        NOT NULL CHECK (kind IN ('device', 'vm')),
    name          text        NOT NULL CHECK (length(name) BETWEEN 1 AND 300),
    manufacturer  text        NULL CHECK (length(manufacturer) <= 200),
    model         text        NULL CHECK (length(model) <= 200),
    serial_number text        NULL CHECK (length(serial_number) <= 200),
    ip            text        NULL CHECK (length(ip) <= 100),
    site          text        NULL CHECK (length(site) <= 200),
    role          text        NULL CHECK (length(role) <= 200),
    platform      text        NULL CHECK (length(platform) <= 200),
    seen_at       timestamptz NOT NULL DEFAULT now(),
    -- Не пришло в последней синхронизации: в источнике удалили.
    missing       boolean     NOT NULL DEFAULT false,
    decision      text        NULL CHECK (decision IN ('accepted', 'rejected')),
    asset_id      uuid        NULL REFERENCES assets (id) ON DELETE SET NULL,
    decided_by    uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    decided_at    timestamptz NULL,
    UNIQUE (source, external_id)
);
CREATE INDEX external_inventory_pending ON external_inventory (name) WHERE decision IS NULL AND NOT missing;
