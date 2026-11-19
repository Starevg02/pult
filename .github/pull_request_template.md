## Что и зачем / What and why

## Как проверено / How it was tested

## Чек-лист / Checklist

- [ ] Включены хуки: `git config core.hooksPath .githooks`; `.githooks/pre-commit` проходит / hooks enabled; pre-commit passes
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace` (с `PULT_TEST_DATABASE_URL`, если меняли базу или API / with `PULT_TEST_DATABASE_URL` if the database or API changed)
- [ ] Фронтенд, если менялся / frontend, if changed: `npm run typecheck`, `npm run lint`, `npm run build` в `web/`
- [ ] TS-клиент перегенерирован после изменения API / TS client regenerated after API changes
- [ ] Новые строки интерфейса и ошибок переведены на ru, en и zh / new UI and error strings translated to ru, en and zh
- [ ] Миграции только новыми файлами / migrations are new files only
- [ ] Заголовки коммитов в Conventional Commits / commit subjects follow Conventional Commits
- [ ] Заметное пользователю изменение описано в `CHANGELOG.md` / user-visible change noted in `CHANGELOG.md`
- [ ] Нет реальных данных организаций: адресов, имён, учёток, ключей, паролей / no real organization data: addresses, names, accounts, keys, passwords
- [ ] Вклад распространяется по лицензии MIT / contribution is licensed under MIT
