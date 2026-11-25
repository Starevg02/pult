-- Обнаруженное оборудование: ПК с агентом попадают в реестр только после проверки инженером.
-- «Ожидают проверки» — активные ПК без решения; здесь хранится только решение.
-- Принятое уже не обновляется само: карточку в реестре меняют вручную.
CREATE TABLE asset_discovery (
    device_id  uuid        PRIMARY KEY REFERENCES devices (id) ON DELETE CASCADE,
    decision   text        NOT NULL CHECK (decision IN ('accepted', 'rejected')),
    -- Карточку удалили — решение уходит с ней, и ПК снова ждёт проверки.
    asset_id   uuid        NULL REFERENCES assets (id) ON DELETE CASCADE,
    decided_by uuid        NULL REFERENCES users (id) ON DELETE SET NULL,
    decided_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX asset_discovery_asset ON asset_discovery (asset_id) WHERE asset_id IS NOT NULL;

-- Тип для ПК из агента — на языке установки (мастер уже мог перевести стартовые типы).
INSERT INTO asset_types (name, fields)
SELECT CASE l WHEN 'en' THEN 'Computer' WHEN 'zh' THEN '电脑' ELSE 'Компьютер' END,
       jsonb_build_array(
           jsonb_build_object('key', 'model', 'type', 'text',
                              'label', CASE l WHEN 'en' THEN 'Model' WHEN 'zh' THEN '型号' ELSE 'Модель' END),
           jsonb_build_object('key', 'os', 'type', 'text',
                              'label', CASE l WHEN 'en' THEN 'OS' WHEN 'zh' THEN '操作系统' ELSE 'ОС' END),
           jsonb_build_object('key', 'ip', 'type', 'text',
                              'label', CASE l WHEN 'en' THEN 'IP address' WHEN 'zh' THEN 'IP 地址' ELSE 'IP-адрес' END))
  FROM (SELECT coalesce((SELECT value->>'default_locale' FROM settings WHERE key = 'locale'), 'ru') AS l) x
ON CONFLICT DO NOTHING;
