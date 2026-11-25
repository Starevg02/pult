-- Оборудование и программы ПК из отчёта агента. Агент присылает их при запуске и когда портал
-- попросит (сведений нет или им больше суток); каждый раз данные целиком заменяются.
CREATE TABLE device_inventory (
    device_id     uuid        PRIMARY KEY REFERENCES devices (id) ON DELETE CASCADE,
    manufacturer  text        NULL,
    model         text        NULL,
    serial_number text        NULL,
    bios_version  text        NULL,
    cpu           text        NULL,
    memory_mb     bigint      NULL CHECK (memory_mb >= 0),
    -- [{"name": "C:", "size_gb": 476, "free_gb": 120}]
    disks         jsonb       NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(disks) = 'array'),
    collected_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE device_software (
    device_id uuid NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    name      text NOT NULL CHECK (length(name) BETWEEN 1 AND 300),
    version   text NULL,
    publisher text NULL
);
CREATE INDEX device_software_device ON device_software (device_id);
-- Поиск «на каких ПК стоит программа» по части названия.
CREATE INDEX device_software_name ON device_software USING gin (lower(name) gin_trgm_ops);
