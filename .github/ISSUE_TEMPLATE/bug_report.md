---
name: Ошибка / Bug report
about: Сообщить об ошибке в Pult / Report a bug in Pult
title: "[Bug] "
labels: bug
assignees: ""
---

<!-- Уязвимости сюда не пишите: см. SECURITY.md. / Do not report vulnerabilities here: see SECURITY.md. -->

## Описание / Description

Что произошло.

## Шаги для воспроизведения / Steps to reproduce

1.
2.
3.

## Ожидаемое поведение / Expected behavior

## Фактическое поведение / Actual behavior

Текст ошибки или скриншот, если есть. / Error text or a screenshot, if any.

## Окружение / Environment

- Версия Pult / Pult version (например / e.g. 0.1.0):
- Способ установки / Installation: `.deb` / Docker Compose / из исходников / from source
- ОС сервера / Server OS:
- ОС ПК, если речь об агенте или helper / Workstation OS, if agent or helper is involved:
- Язык интерфейса / UI language: ru / en / zh
- Браузер / Browser:

## Журналы / Logs

Журнал сервера: `sudo journalctl -u pult` (пакет) или `docker compose logs` (Docker).
Server log: `sudo journalctl -u pult` (package) or `docker compose logs` (Docker).

**Не вставляйте пароли, ключи (`PULT_SECRET_KEY`), токены, адреса и данные сотрудников. / Do not paste passwords, keys (`PULT_SECRET_KEY`), tokens, addresses or employee data.**
