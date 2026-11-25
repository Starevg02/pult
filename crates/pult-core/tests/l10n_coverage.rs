//! Каждая русская строка сервера, которую видит пользователь, есть в каталоге `l10n/en.tsv`.
//!
//! Строки собираются из исходников `crates/*/src`. Не проверяются: модули тестов (от `#[cfg(test)]`
//! до конца файла), комментарии, журнал (`tracing`), `expect`/`assert`/`panic`, ошибки CLI (`anyhow`,
//! `bail!`, `context`), описания OpenAPI (`description = "…"`), русская половина `l("…", "…")` в письмах
//! и файлы, которые пользователь портала не видит (агент, helper, CLI и миграции, демо, тестовый набор).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{collections::BTreeSet, fs, path::Path};

/// Файлы и каталоги вне портала: CLI, агент и helper Windows, тестовый набор.
const SKIP: &[&str] = &[
    "pult-agent/",
    "pult-helper/",
    "pult-testkit/",
    "pult-server/src/main.rs",
    "pult-server/src/backup.rs",
    "pult-server/src/migrate.rs",
    "pult-server/src/demo.rs",
    "pult-server/src/edge.rs",
    "pult-server/src/telemetry.rs",
    "pult-server/src/config.rs",
    "pult-server/src/static_files.rs",
    "pult-phonebook/src/reader.rs",
    "pult-phonebook/src/import.rs",
];

/// Не текст для людей: раскладка клавиатуры и «ё→е» для поиска, образец текста ошибки LDAP,
/// «да/нет» в ячейках импорта CSV.
const SKIP_LITERAL: &[&str] = &["е", "йцукенгшщзхъфывапролджэячсмитьбюё", "код ldap 49", "да", "нет"];

const SKIP_LINE: &[&str] = &[
    "expect(",
    "assert",
    "panic!",
    "unreachable!",
    "anyhow!",
    "bail!",
    "context(",
    "with_context(",
    "description = \"",
    "#[doc",
];

fn is_cyrillic(c: char) -> bool {
    pult_core::l10n::is_cyrillic(c)
}

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Текст файла без тестов, комментариев, журнала и строк из `SKIP_LINE`.
fn cleaned(src: &str) -> String {
    let src = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let mut out = String::new();
    let mut in_log = false;
    for line in src.lines() {
        let t = line.trim_start();
        let log_start =
            ["tracing::", "info!(", "warn!(", "error!(", "debug!(", "trace!("].iter().any(|m| t.contains(m))
                && !t.contains("#[error(");
        if log_start {
            in_log = true;
        }
        let skip = in_log || t.starts_with("//") || SKIP_LINE.iter().any(|m| t.contains(m));
        if in_log && (t.ends_with(';') || t.ends_with("});") || t.ends_with(");")) {
            in_log = false;
        }
        if !skip {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Строковые литералы с кириллицей; русская половина `l("…", "…")` пропускается.
fn literals(text: &str) -> Vec<String> {
    let b: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // Символьный литерал '"' / '\'' — не начало строки.
            '\'' if i + 2 < b.len()
                && (b[i + 2] == '\'' || (b[i + 1] == '\\' && i + 3 < b.len() && b[i + 3] == '\'')) =>
            {
                i += if b[i + 1] == '\\' { 4 } else { 3 };
            }
            '/' if b.get(i + 1) == Some(&'/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '"' => {
                let before: String = b[..i].iter().rev().take(80).collect::<String>().chars().rev().collect();
                let first_of_l = before.trim_end().ends_with("l(");
                let mut s = String::new();
                i += 1;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' && i + 1 < b.len() {
                        match b[i + 1] {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            '"' => s.push('"'),
                            '\\' => s.push('\\'),
                            '\n' => {
                                // Продолжение строки: перевод строки и отступ не входят в строку.
                                i += 2;
                                while i < b.len() && b[i].is_whitespace() {
                                    i += 1;
                                }
                                continue;
                            }
                            c => {
                                s.push('\\');
                                s.push(c);
                            }
                        }
                        i += 2;
                        continue;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                i += 1;
                if first_of_l || !s.chars().any(is_cyrillic) || SKIP_LITERAL.contains(&s.as_str()) {
                    continue;
                }
                if ["SELECT ", "INSERT INTO", "UPDATE ", "DELETE FROM"].iter().any(|k| s.contains(k)) {
                    // SQL: переводятся только русские значения в кавычках.
                    out.extend(
                        s.split('\'').skip(1).step_by(2).filter(|v| v.chars().any(is_cyrillic)).map(String::from),
                    );
                } else {
                    out.push(s);
                }
            }
            _ => i += 1,
        }
    }
    out
}

#[test]
fn every_user_facing_string_is_translated() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    let known: BTreeSet<&str> = pult_core::l10n::entries().map(|(ru, _)| ru).collect();
    let mut missing = BTreeSet::new();
    let mut used = BTreeSet::new();
    for f in files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        if !rel.contains("/src/") || SKIP.iter().any(|s| rel.starts_with(s)) {
            continue;
        }
        for s in literals(&cleaned(&fs::read_to_string(&f).unwrap())) {
            if known.contains(s.as_str()) {
                used.insert(s);
            } else {
                missing.insert(format!("{}\t{rel}", s.replace('\n', "\\n")));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "нет в crates/pult-core/src/l10n/en.tsv ({}):\n{}",
        missing.len(),
        missing.into_iter().collect::<Vec<_>>().join("\n")
    );
    // Строки, которых в коде больше нет, из каталога убираем.
    let stale: Vec<_> = known.iter().filter(|k| !used.contains(**k)).collect();
    assert!(stale.is_empty(), "в каталоге лишние строки: {stale:?}");
}

/// Русские половины `l("ru", "en")` в письмах (английская стоит в коде).
fn mail_pairs() -> BTreeSet<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pult-helpdesk/src/notify.rs");
    let src = fs::read_to_string(root).unwrap();
    let mut out = BTreeSet::new();
    let mut rest = src.as_str();
    while let Some(at) = rest.find("l(") {
        // `l(` — отдельное слово, не конец другого имени (`fill(`); строка — после пробелов и переводов строк.
        let before = rest[..at].chars().next_back();
        rest = &rest[at + 2..];
        let arg = rest.trim_start();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') || !arg.starts_with('"') {
            continue;
        }
        let arg = &arg[1..];
        let end = arg.find('"').unwrap();
        out.insert(arg[..end].to_string());
    }
    out
}

#[test]
fn chinese_catalog_matches_english_and_mail() {
    let zh: BTreeSet<&str> = pult_core::l10n::entries_for(pult_core::l10n::Lang::Zh).map(|(ru, _)| ru).collect();
    let mut need: BTreeSet<String> = pult_core::l10n::entries().map(|(ru, _)| ru.to_string()).collect();
    let mail = mail_pairs();
    assert!(mail.len() > 30, "письма найдены: {}", mail.len());
    need.extend(mail);
    let missing: Vec<_> = need.iter().filter(|s| !zh.contains(s.as_str())).collect();
    assert!(missing.is_empty(), "нет в crates/pult-core/src/l10n/zh.tsv: {missing:?}");
    let stale: Vec<_> = zh.iter().filter(|s| !need.contains(**s)).collect();
    assert!(stale.is_empty(), "в zh.tsv лишние строки: {stale:?}");
}

#[test]
fn extraction_rules() {
    let src = "let a = \"Ошибка {x}\";\n// \"коммент\"\ntracing::info!(\n  \"журнал\",\n);\nl(\"рус\", \"en\");\nlet c = '\"';\nlet d = \"строка \\\n    дальше\";\n#[cfg(test)]\nmod tests { \"тест\" }";
    assert_eq!(literals(&cleaned(src)), vec!["Ошибка {x}".to_string(), "строка дальше".to_string()]);
}
