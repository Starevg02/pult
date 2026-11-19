//! Всё, что сервер кладёт в ленту заявки и в уведомления, подписано в интерфейсе на обоих языках
//! (`web/src/i18n/{ru,en}.json`): события ленты — `helpdesk.feed.<вид>`, уведомления —
//! `notifications.kind.<вид>`. Без подписи лента показывает код вида (так было с `access.sent`),
//! а уведомление — безликое «Уведомление» (так было с `helpdesk.approval` и сроками SLA).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{collections::BTreeSet, fs, path::Path};

/// Первый литерал вида `a.b_c` (или `a_b` при `dotted = false`) в каждом вызове `name(&mut …` / `name(db, …`
/// до `.await`. Определения функций и модули тестов пропускаются.
fn call_kinds(src: &str, name: &str, dotted: bool) -> BTreeSet<String> {
    let src = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let open = format!("{name}(");
    let mut out = BTreeSet::new();
    for (i, _) in src.match_indices(&open) {
        let before = &src[..i];
        if before.ends_with("fn ") || before.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let call = &src[i + open.len()..];
        let call = &call[..call.find(".await").unwrap_or(call.len())];
        let lit = call.split('"').skip(1).step_by(2).find(|s| {
            !s.is_empty()
                && (!dotted || s.contains('.'))
                && s.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '.')
        });
        if let Some(k) = lit {
            out.insert(k.to_string());
        }
    }
    out
}

/// Виды событий ленты: `event(&mut tx, …, "вид", …)`.
fn feed_kinds(src: &str) -> BTreeSet<String> {
    let src = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let mut out = BTreeSet::new();
    for (i, _) in src.match_indices("event(") {
        let call = &src[i + "event(".len()..];
        if !call.trim_start().starts_with("&mut") || src[..i].ends_with("fn ") {
            continue;
        }
        let call = &call[..call.find(".await").unwrap_or(call.len())];
        let lit = call
            .split('"')
            .skip(1)
            .step_by(2)
            .find(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '.'));
        if let Some(k) = lit {
            out.insert(k.to_string());
        }
    }
    out
}

/// Виды уведомлений: вызовы `bell`, `to_user`, `notify`, `notify_superadmins` и константы `notify::kind`.
fn notification_kinds(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in ["bell", "to_user", "notify", "notify_superadmins"] {
        out.extend(call_kinds(src, f, true));
    }
    if let Some(m) = src.split("pub mod kind {").nth(1) {
        let m = &m[..m.find('}').unwrap_or(m.len())];
        out.extend(m.split('"').skip(1).step_by(2).map(str::to_string));
    }
    out
}

fn sources(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(fs::read_to_string(&p).unwrap());
            }
        }
    }
    let mut out = Vec::new();
    for c in fs::read_dir(root.join("crates")).unwrap().flatten() {
        let src = c.path().join("src");
        if src.is_dir() {
            walk(&src, &mut out);
        }
    }
    out
}

fn lookup<'a>(json: &'a serde_json::Value, prefix: &[&str], kind: &str) -> Option<&'a str> {
    let base = prefix.iter().try_fold(json, |v, k| v.get(*k))?;
    kind.split('.').try_fold(base, |v, k| v.get(k))?.as_str()
}

#[test]
fn every_feed_event_and_notification_has_a_label() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (mut feed, mut notes) = (BTreeSet::new(), BTreeSet::new());
    for s in sources(&root) {
        feed.extend(feed_kinds(&s));
        notes.extend(notification_kinds(&s));
    }
    for k in ["created", "access.sent", "directory.operation"] {
        assert!(feed.contains(k), "вид ленты {k} не найден: {feed:?}");
    }
    for k in ["ad.sync_failed", "helpdesk.approval", "helpdesk.sla_breached", "system.backup_failed"] {
        assert!(notes.contains(k), "вид уведомления {k} не найден: {notes:?}");
    }
    for lang in ["ru", "en"] {
        let p = root.join(format!("web/src/i18n/{lang}.json"));
        let json: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        let no_feed: Vec<_> = feed.iter().filter(|k| lookup(&json, &["helpdesk", "feed"], k).is_none()).collect();
        let no_note: Vec<_> = notes.iter().filter(|k| lookup(&json, &["notifications", "kind"], k).is_none()).collect();
        assert!(
            no_feed.is_empty() && no_note.is_empty(),
            "{lang}.json: нет helpdesk.feed для {no_feed:?}, notifications.kind для {no_note:?}"
        );
    }
}

#[test]
fn kinds_are_read_from_calls() {
    let src = "event(&mut tx, t.id, who, \"access.sent\", Value::Null, json!({\"owner\": o})).await?;\n\
               tickets::event(\n  &mut tx,\n  id,\n  None,\n  \"directory.operation\",\n).await?;\n\
               event(&mut tx, id, who, kind, a, b).await?;\n\
               notify::bell(&mut tx, u, \"helpdesk.approval\", &t).await?;\n\
               pult_core::notify::notify_superadmins(\n  &mut *tx,\n  \"helpdesk.sla_breached\",\n  json!({\"number\": 1}),\n).await?;\n\
               notify::ctx(&core).await?;\n\
               pub mod kind {\n    pub const A: &str = \"ad.sync_failed\";\n}\n\
               pub async fn notify(db: E, kind: &str) {}";
    assert_eq!(feed_kinds(src).into_iter().collect::<Vec<_>>(), ["access.sent", "directory.operation"]);
    assert_eq!(
        notification_kinds(src).into_iter().collect::<Vec<_>>(),
        ["ad.sync_failed", "helpdesk.approval", "helpdesk.sla_breached"]
    );
}
