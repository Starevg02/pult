#!/bin/bash
# Тестовые учётки для проверки входа: по одной на каждый ответ экрана входа.
# Пароли — только для тестового домена в compose.dev.yml.
set -uo pipefail

until samba-tool user list >/dev/null 2>&1; do sleep 2; done
[ -f /var/lib/samba/.seeded ] && exit 0

PW="${TEST_USER_PASSWORD:-Pult-Test-2026!}"
mk() { samba-tool user create "$@" || true; }

# Служебная учётка портала: только чтение (обычный пользователь домена читает каталог).
mk svc-pult "${SVC_PASSWORD:-Pult-Svc-2026!}" --description="Pult: чтение каталога"

mk ivanov    "$PW" --given-name="Иван"  --surname="Иванов"   --mail-address=ivanov@pult.test  --job-title="Бухгалтер"  --department="Бухгалтерия" --telephone-number=4512
mk petrova   "$PW" --given-name="Анна"  --surname="Петрова"  --mail-address=petrova@pult.test --job-title="Инженер L1" --department="IT-служба"
mk disabled  "$PW" --given-name="Отключённый" --surname="Пользователь"
samba-tool user disable disabled || true                      # → data 533
mk mustchange "$PW" --must-change-at-next-login               # → data 773
mk expired   "$PW"
samba-tool user setexpiry expired --days=0 || true           # → data 701 (истёк срок учётки)

touch /var/lib/samba/.seeded
echo "Тестовые пользователи созданы: ivanov, petrova, disabled, mustchange, expired, svc-pult"
