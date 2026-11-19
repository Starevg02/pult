-- Компьютеры домена: имя ПК в заявке и поиск для инженеров.
-- Адреса — из прямых записей DNS: в объекте компьютера в AD IP-адреса нет.
CREATE TABLE ad_computers (
    id               uuid        PRIMARY KEY DEFAULT uuid_v7(),
    domain_id        uuid        NOT NULL REFERENCES ad_domains (id) ON DELETE CASCADE,
    object_guid      uuid        NOT NULL,
    name             text        NOT NULL,
    dns_host_name    text,
    description      text,
    operating_system text,
    is_enabled       boolean     NOT NULL,
    addresses        inet[]      NOT NULL DEFAULT '{}',
    synced_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (domain_id, object_guid)
);
