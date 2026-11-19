//! Язык ответа сервера: русский — исходный, английский и китайский — по каталогам
//! `l10n/en.tsv` и `l10n/zh.tsv`.
//!
//! Язык запроса задаёт middleware [`scope`]: cookie `pult_lang` (ставит интерфейс, она же приходит со
//! ссылками на выгрузку), иначе первый тег `Accept-Language`, иначе русский. Внутри запроса [`tr`]
//! переводит готовую строку целиком: сообщения об ошибках, подписи отчётов, описания прав.
//! Строка из `format!` переводится по шаблону: `{…}` в ключе каталога совпадает с любым текстом и
//! подставляется в перевод (по имени, а безымянные `{}` — по порядку).
//!
//! Фоновые задачи языка запроса не знают: письма выбирают язык получателя сами (`users.locale`).
//! Журнал сервера (`tracing`) и команды CLI — на русском.

use axum::{extract::Request, middleware::Next, response::Response};
use std::{borrow::Cow, collections::HashMap, sync::LazyLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Ru,
    En,
    /// Китайский (упрощённый).
    Zh,
}

impl Lang {
    /// Коды языков, которые принимают настройки и учётки.
    pub const CODES: &'static [&'static str] = &["ru", "en", "zh"];

    /// `en`, `en-US`, `EN` → английский; `zh`, `zh-CN`, `zh-Hans` → китайский; всё остальное — русский.
    pub fn parse(s: &str) -> Self {
        let tag = s.trim().split(['-', '_', ';', ',']).next().unwrap_or("");
        if tag.eq_ignore_ascii_case("en") {
            Self::En
        } else if tag.eq_ignore_ascii_case("zh") {
            Self::Zh
        } else {
            Self::Ru
        }
    }

    /// Код из списка [`Lang::CODES`] (без разбора тегов браузера).
    pub fn is_code(s: &str) -> bool {
        Self::CODES.contains(&s)
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
            Self::Zh => "zh",
        }
    }
}

tokio::task_local! {
    static LANG: Lang;
}

/// Язык текущего запроса (вне запроса — русский).
pub fn lang() -> Lang {
    LANG.try_with(|l| *l).unwrap_or_default()
}

/// Выполнить `f` с языком `lang` (фоновые задачи, тесты).
pub async fn with_lang<F: std::future::Future>(lang: Lang, f: F) -> F::Output {
    LANG.scope(lang, f).await
}

/// Язык установки по умолчанию (настройка `locale`).
pub async fn default_lang(db: &sqlx::PgPool) -> Lang {
    crate::settings::load::<crate::settings::LocaleSettings>(db)
        .await
        .map(|s| Lang::parse(&s.default_locale))
        .unwrap_or_default()
}

/// Язык из заголовков запроса.
pub fn from_headers(h: &axum::http::HeaderMap) -> Lang {
    let cookie = h
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix("pult_lang="));
    if let Some(c) = cookie {
        return Lang::parse(c);
    }
    h.get(axum::http::header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()).map(Lang::parse).unwrap_or_default()
}

/// Middleware: язык запроса для всего обработчика.
pub async fn scope(req: Request, next: Next) -> Response {
    let lang = from_headers(req.headers());
    LANG.scope(lang, next.run(req)).await
}

/// Перевести строку на язык запроса. Нет в каталоге — вернуть как есть.
pub fn tr(s: &str) -> Cow<'_, str> {
    tr_to(lang(), s)
}

/// Перевести строку на заданный язык.
pub fn tr_to(lang: Lang, s: &str) -> Cow<'_, str> {
    if lang == Lang::Ru || s.is_empty() || !s.chars().any(is_cyrillic) {
        return Cow::Borrowed(s);
    }
    let c = match lang {
        Lang::Zh => &*ZH_CATALOG,
        _ => &*CATALOG,
    };
    if let Some(en) = c.exact.get(s) {
        return Cow::Owned((*en).to_string());
    }
    for t in &c.templates {
        if let Some(out) = t.apply(s, lang) {
            return Cow::Owned(out);
        }
    }
    Cow::Borrowed(s)
}

/// Перевести строки внутри JSON (только для показа: сохранённые данные не меняются).
pub fn tr_values(v: &mut serde_json::Value) {
    if lang() == Lang::Ru {
        return;
    }
    match v {
        serde_json::Value::String(s) => {
            if let Cow::Owned(t) = tr(s) {
                *s = t;
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(tr_values),
        serde_json::Value::Object(o) => o.values_mut().for_each(tr_values),
        _ => {}
    }
}

pub fn is_cyrillic(c: char) -> bool {
    matches!(c, 'А'..='я' | 'Ё' | 'ё')
}

/// Каталог: строка `русский<TAB>перевод`; строки без табуляции — пояснения.
const EN: &str = include_str!("l10n/en.tsv");
/// Китайский: те же строки, что в `en.tsv`, и русские половины `l("…", "…")` из писем
/// (английский их перевод стоит прямо в коде).
const ZH: &str = include_str!("l10n/zh.tsv");

struct Catalog {
    exact: HashMap<&'static str, &'static str>,
    /// Шаблоны с `{…}`; длинные — раньше (точнее совпадение).
    templates: Vec<Template>,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| build(entries()));
static ZH_CATALOG: LazyLock<Catalog> = LazyLock::new(|| build(entries_for(Lang::Zh)));

fn build(list: impl Iterator<Item = (&'static str, &'static str)>) -> Catalog {
    let mut exact = HashMap::new();
    let mut templates = Vec::new();
    for (ru, en) in list {
        match Template::parse(ru, en) {
            Some(t) => templates.push(t),
            None => {
                exact.insert(ru, en);
            }
        }
    }
    templates.sort_by_key(|t| std::cmp::Reverse(t.literal_len()));
    Catalog { exact, templates }
}

fn parse_tsv(text: &'static str) -> Vec<(&'static str, &'static str)> {
    text.lines()
        // Запись — строка с табуляцией; остальное (пустые строки, пояснения) пропускается.
        .filter_map(|l| l.split_once('\t'))
        .map(|(ru, tr)| (unescape(ru), unescape(tr)))
        .collect()
}

static ENTRIES: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| parse_tsv(EN));
static ZH_ENTRIES: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| parse_tsv(ZH));

/// Пары английского каталога (для проверки полноты в тестах).
pub fn entries() -> impl Iterator<Item = (&'static str, &'static str)> {
    ENTRIES.iter().copied()
}

/// Пары каталога языка (у русского — пусто).
pub fn entries_for(lang: Lang) -> impl Iterator<Item = (&'static str, &'static str)> {
    match lang {
        Lang::Ru => [].iter(),
        Lang::En => ENTRIES.iter(),
        Lang::Zh => ZH_ENTRIES.iter(),
    }
    .copied()
}

/// `\n` в каталоге — перевод строки (в TSV он разорвал бы запись). Разбирается один раз.
fn unescape(s: &'static str) -> &'static str {
    if s.contains("\\n") { Box::leak(s.replace("\\n", "\n").into_boxed_str()) } else { s }
}

#[derive(Debug, PartialEq)]
enum Part {
    Lit(String),
    Hole(String),
}

/// Разбор `format!`-строки: `{{`/`}}` — фигурные скобки, `{имя}`/`{}`/`{:?}` — подстановка.
fn parts(s: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut it = s.chars().peekable();
    let mut n = 0;
    while let Some(c) = it.next() {
        match c {
            '{' if it.peek() == Some(&'{') => {
                it.next();
                lit.push('{');
            }
            '}' if it.peek() == Some(&'}') => {
                it.next();
                lit.push('}');
            }
            '{' => {
                let mut name = String::new();
                for c in it.by_ref() {
                    if c == '}' {
                        break;
                    }
                    name.push(c);
                }
                let name = name.split(':').next().unwrap_or("").trim().to_string();
                let name = if name.is_empty() {
                    n += 1;
                    format!("#{n}")
                } else {
                    name
                };
                if !lit.is_empty() {
                    out.push(Part::Lit(std::mem::take(&mut lit)));
                }
                out.push(Part::Hole(name));
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() {
        out.push(Part::Lit(lit));
    }
    out
}

struct Template {
    from: Vec<Part>,
    to: Vec<Part>,
}

impl Template {
    fn parse(ru: &str, en: &str) -> Option<Self> {
        let from = parts(ru);
        if !from.iter().any(|p| matches!(p, Part::Hole(_))) {
            return None;
        }
        Some(Self { from, to: parts(en) })
    }

    fn literal_len(&self) -> usize {
        self.from.iter().map(|p| if let Part::Lit(l) = p { l.len() } else { 0 }).sum()
    }

    /// Совпадение строки с шаблоном: литералы — по порядку, подстановки — непустой текст между ними.
    fn apply(&self, s: &str, lang: Lang) -> Option<String> {
        let mut vals: Vec<(&str, &str)> = Vec::new();
        let mut rest = s;
        let mut pending: Option<&str> = None;
        for (i, p) in self.from.iter().enumerate() {
            match p {
                Part::Lit(l) => {
                    if let Some(name) = pending.take() {
                        // Последний литерал прижимаем к концу строки, остальные — к первому вхождению.
                        let last = self.from[i + 1..].iter().all(|p| matches!(p, Part::Lit(_)));
                        let at = if last { rest.rfind(l.as_str())? } else { rest.find(l.as_str())? };
                        if at == 0 {
                            return None;
                        }
                        vals.push((name, &rest[..at]));
                        rest = &rest[at + l.len()..];
                    } else {
                        rest = rest.strip_prefix(l.as_str())?;
                    }
                }
                Part::Hole(name) => pending = Some(name),
            }
        }
        match pending {
            Some(name) if !rest.is_empty() => vals.push((name, rest)),
            Some(_) => return None,
            None if !rest.is_empty() => return None,
            None => {}
        }
        let mut out = String::new();
        for p in &self.to {
            match p {
                Part::Lit(l) => out.push_str(l),
                Part::Hole(name) => {
                    let v = vals.iter().find(|(n, _)| n == name)?.1;
                    // Подставленный текст тоже может быть из каталога (название статуса и т. п.).
                    out.push_str(&tr_to(lang, v));
                }
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ru: &str, en: &str, s: &str) -> Option<String> {
        Template::parse(ru, en).and_then(|t| t.apply(s, Lang::En))
    }

    #[test]
    fn language_from_request() {
        let mut h = axum::http::HeaderMap::new();
        assert_eq!(from_headers(&h), Lang::Ru);
        h.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
        assert_eq!(from_headers(&h), Lang::En);
        h.insert("cookie", "a=1; pult_lang=ru".parse().unwrap());
        assert_eq!(from_headers(&h), Lang::Ru, "выбор в интерфейсе важнее языка браузера");
    }

    #[test]
    fn templates() {
        assert_eq!(
            t("Заполните поле «{}».", "Fill in “{}”.", "Заполните поле «ФИО».").as_deref(),
            Some("Fill in “ФИО”.")
        );
        assert_eq!(
            t("По заявке #{n}: {who}", "{who} on #{n}", "По заявке #12: Иван").as_deref(),
            Some("Иван on #12"),
            "по имени, в другом порядке"
        );
        assert_eq!(
            t("Файл {} больше {} МБ", "File {} is over {} MB", "Файл a.png больше 10 МБ").as_deref(),
            Some("File a.png is over 10 MB")
        );
        assert_eq!(t("Нет {}.", "No {}.", "Нет ."), None, "пустая подстановка — не совпадение");
        assert_eq!(t("Нет {}.", "No {}.", "Есть x."), None);
        assert_eq!(parts("{{x}} {a:?}"), vec![Part::Lit("{x} ".into()), Part::Hole("a".into())]);
    }

    #[tokio::test]
    async fn translation_follows_request_language() {
        let ru = "недостаточно прав";
        assert_eq!(tr(ru), ru, "вне запроса — как есть");
        let en = with_lang(Lang::En, async { tr(ru).into_owned() }).await;
        assert_eq!(en, "insufficient permissions");
        assert_eq!(tr_to(Lang::En, "no cyrillic"), "no cyrillic");
    }

    #[test]
    fn catalog_is_well_formed() {
        for lang in [Lang::En, Lang::Zh] {
            let mut seen = std::collections::HashSet::new();
            for (ru, tr) in entries_for(lang) {
                assert!(seen.insert(ru), "повтор в каталоге {lang:?}: {ru}");
                assert!(!tr.trim().is_empty() && !tr.chars().any(is_cyrillic), "нет перевода {lang:?}: {ru}");
                if let Some(t) = Template::parse(ru, tr) {
                    let holes = |p: &[Part]| {
                        let mut v: Vec<_> = p
                            .iter()
                            .filter_map(|p| if let Part::Hole(h) = p { Some(h.clone()) } else { None })
                            .collect();
                        v.sort();
                        v
                    };
                    assert_eq!(holes(&t.from), holes(&t.to), "подстановки не совпадают {lang:?}: {ru}");
                }
            }
        }
    }

    #[test]
    fn chinese_covers_english_catalog() {
        let zh: std::collections::HashSet<_> = entries_for(Lang::Zh).map(|(ru, _)| ru).collect();
        let missing: Vec<_> = entries().map(|(ru, _)| ru).filter(|ru| !zh.contains(ru)).collect();
        assert!(missing.is_empty(), "нет в zh.tsv: {missing:?}");
    }

    #[tokio::test]
    async fn chinese_language() {
        assert_eq!(Lang::parse("zh-CN,zh;q=0.9"), Lang::Zh);
        assert_eq!(Lang::parse("zh-Hans"), Lang::Zh);
        assert!(Lang::is_code("zh") && !Lang::is_code("zh-CN"));
        let zh = with_lang(Lang::Zh, async { tr("недостаточно прав").into_owned() }).await;
        assert!(!zh.chars().any(is_cyrillic) && !zh.is_ascii(), "{zh}");
    }
}
