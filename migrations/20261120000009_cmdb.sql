-- CMDB: связи между объектами каталога, жизненный цикл по типам и учёт лицензий ПО.

-- Виды связей задаёт администратор. «Влияние»: отказ объекта, на который указывает связь,
-- затрагивает объект, от которого она идёт (сервис «зависит от» сервера).
CREATE TABLE asset_relation_types (
    id           uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name         text        NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    reverse_name text        NOT NULL CHECK (length(reverse_name) BETWEEN 1 AND 100),
    impact       boolean     NOT NULL DEFAULT true,
    is_active    boolean     NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX asset_relation_types_name ON asset_relation_types (lower(name));
-- Стартовые названия — на языке установки (мастер мог уже перевести стартовые данные).
INSERT INTO asset_relation_types (name, reverse_name, impact)
SELECT v.name[i], v.reverse[i], v.impact
  FROM (SELECT CASE coalesce((SELECT value->>'default_locale' FROM settings WHERE key = 'locale'), 'ru')
               WHEN 'en' THEN 2 WHEN 'zh' THEN 3 ELSE 1 END AS i) l,
       (VALUES (ARRAY['Зависит от', 'Depends on', '依赖于'], ARRAY['Нужен для', 'Required by', '被依赖于'], true, 1),
               (ARRAY['Подключено к', 'Connected to', '连接到'], ARRAY['К нему подключено', 'Connected to it', '连接到此设备'], true, 2),
               (ARRAY['Установлено на', 'Installed on', '安装在'], ARRAY['На нём установлено', 'Installed on it', '其上安装'], true, 3),
               (ARRAY['Входит в', 'Part of', '属于'], ARRAY['Состоит из', 'Consists of', '由以下组成'], false, 4))
         AS v (name, reverse, impact, ord)
 ORDER BY v.ord;

CREATE TABLE asset_relations (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    from_id    uuid        NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    to_id      uuid        NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    type_id    uuid        NOT NULL REFERENCES asset_relation_types (id),
    created_by uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (from_id <> to_id),
    UNIQUE (from_id, to_id, type_id)
);
CREATE INDEX asset_relations_to ON asset_relations (to_id);

-- Сервисы и информационные системы — тоже объекты CMDB.
INSERT INTO asset_types (name, fields)
SELECT CASE l WHEN 'en' THEN 'Service / IS' WHEN 'zh' THEN '服务 / 信息系统' ELSE 'Сервис / ИС' END,
       jsonb_build_array(
           jsonb_build_object('key', 'owner', 'type', 'text',
                              'label', CASE l WHEN 'en' THEN 'Owner' WHEN 'zh' THEN '负责人' ELSE 'Владелец' END),
           jsonb_build_object('key', 'url', 'type', 'text',
                              'label', CASE l WHEN 'en' THEN 'Address' WHEN 'zh' THEN '地址' ELSE 'Адрес' END))
  FROM (SELECT coalesce((SELECT value->>'default_locale' FROM settings WHERE key = 'locale'), 'ru') AS l) x
ON CONFLICT DO NOTHING;
INSERT INTO asset_types (name, fields)
SELECT CASE l WHEN 'en' THEN 'Server' WHEN 'zh' THEN '服务器' ELSE 'Сервер' END,
       jsonb_build_array(
           jsonb_build_object('key', 'model', 'type', 'text',
                              'label', CASE l WHEN 'en' THEN 'Model' WHEN 'zh' THEN '型号' ELSE 'Модель' END),
           jsonb_build_object('key', 'ip', 'type', 'text',
                              'label', CASE l WHEN 'en' THEN 'IP address' WHEN 'zh' THEN 'IP 地址' ELSE 'IP-адрес' END))
  FROM (SELECT coalesce((SELECT value->>'default_locale' FROM settings WHERE key = 'locale'), 'ru') AS l) x
ON CONFLICT DO NOTHING;

-- Жизненный цикл типа: какие состояния у него бывают и какие переходы разрешены.
-- {"statuses": ["in_use", "in_stock", "repair", "retired"], "transitions": [["in_stock", "in_use"], …]};
-- пустой список переходов — любые.
ALTER TABLE asset_types ADD COLUMN lifecycle jsonb NOT NULL
    DEFAULT '{"statuses": ["in_use", "in_stock", "repair", "retired"], "transitions": []}'
    CHECK (jsonb_typeof(lifecycle -> 'statuses') = 'array' AND jsonb_typeof(lifecycle -> 'transitions') = 'array');

-- Лицензии ПО. Использование считается по программам из отчётов агента: название программы
-- содержит строку «pattern» (без учёта регистра).
CREATE TABLE software_licenses (
    id         uuid        PRIMARY KEY DEFAULT uuid_v7(),
    name       text        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    vendor     text        NULL CHECK (length(vendor) <= 200),
    pattern    text        NOT NULL CHECK (length(pattern) BETWEEN 2 AND 200),
    -- NULL — без ограничения числа установок.
    seats      integer     NULL CHECK (seats >= 0),
    expires_on date        NULL,
    notes      text        NULL CHECK (length(notes) <= 2000),
    is_active  boolean     NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX software_licenses_name ON software_licenses (lower(name));

-- Мониторинг ↔ CMDB. Оповещение относится к объекту каталога, если узел из оповещения — его название
-- (или короткое имя из FQDN), IP из поля «ip» или имя/адрес принятого в каталог ПК с агентом.
-- «Адрес:порт» от Prometheus сравнивается по адресу.
CREATE VIEW alert_assets AS
SELECT al.id AS alert_id, a.id AS asset_id
  FROM alerts al
  JOIN assets a ON a.status <> 'retired' AND (
           lower(a.name) = lower(al.host)
        OR lower(a.name) = lower(split_part(al.host, '.', 1))
        OR a.attributes ->> 'ip' = split_part(al.host, ':', 1)
        OR EXISTS (SELECT 1 FROM asset_discovery ad JOIN devices d ON d.id = ad.device_id
                    WHERE ad.asset_id = a.id
                      AND (lower(d.hostname) = lower(split_part(al.host, '.', 1)) OR lower(d.fqdn) = lower(al.host)
                           OR split_part(al.host, ':', 1) = ANY (SELECT host(x) FROM unnest(d.ip_addrs) x))))
 WHERE al.status = 'firing' AND al.host IS NOT NULL;

-- Типы, объекты которых показываются в «Состоянии сервисов».
ALTER TABLE asset_types ADD COLUMN show_status boolean NOT NULL DEFAULT false;
UPDATE asset_types SET show_status = true WHERE name IN ('Сервис / ИС', 'Service / IS', '服务 / 信息系统');
