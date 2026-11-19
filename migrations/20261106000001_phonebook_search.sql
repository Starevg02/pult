-- Поиск в справочнике. Поисковый текст считает БД — переименование
-- подразделения или должности сразу отражается в поиске, и приложение не может «забыть» его обновить.

-- Нижний регистр по правилам Unicode (ICU — не зависит от локали кластера), ё → е, пробелы схлопнуты.
-- Та же нормализация запроса — в pult-phonebook (search::normalize).
CREATE FUNCTION pult_norm(t text) RETURNS text
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT btrim(regexp_replace(replace(lower(coalesce(t, '') COLLATE "und-x-icu"), 'ё', 'е'), '\s+', ' ', 'g'))
$$;

CREATE FUNCTION pult_digits(t text) RETURNS text
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT regexp_replace(coalesce(t, ''), '\D', '', 'g')
$$;

CREATE FUNCTION persons_search_fill() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.search_text := pult_norm(concat_ws(' ',
        NEW.last_name, NEW.first_name, NEW.middle_name,
        (SELECT name FROM positions WHERE id = NEW.position_id),
        (SELECT name FROM departments WHERE id = NEW.department_id),
        NEW.email, NEW.work_phone, NEW.internal_ext, NEW.mobile_phone, NEW.room,
        (SELECT name FROM sites WHERE id = NEW.site_id)));
    NEW.phone_digits := btrim(concat_ws(' ', pult_digits(NEW.work_phone), pult_digits(NEW.internal_ext), pult_digits(NEW.mobile_phone)));
    RETURN NEW;
END
$$;
CREATE TRIGGER persons_search BEFORE INSERT OR UPDATE ON persons FOR EACH ROW EXECUTE FUNCTION persons_search_fill();

-- Переименовали подразделение, должность или площадку — пересчитать поиск у сотрудников.
CREATE FUNCTION persons_search_refresh() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.name IS DISTINCT FROM OLD.name THEN
        EXECUTE format('UPDATE persons SET search_text = search_text WHERE %I = $1', TG_ARGV[0]) USING NEW.id;
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER departments_search AFTER UPDATE OF name ON departments
    FOR EACH ROW EXECUTE FUNCTION persons_search_refresh('department_id');
CREATE TRIGGER positions_search AFTER UPDATE OF name ON positions
    FOR EACH ROW EXECUTE FUNCTION persons_search_refresh('position_id');
CREATE TRIGGER sites_search AFTER UPDATE OF name ON sites
    FOR EACH ROW EXECUTE FUNCTION persons_search_refresh('site_id');
