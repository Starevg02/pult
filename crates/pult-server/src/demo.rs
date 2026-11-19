//! Демо-режим (`cargo run -p pult-server --features demo -- demo`): посмотреть портал, пройти мастер
//! и снять снимки экрана без настоящего домена. Поддельный контроллер домена (LDAPS со своим CA)
//! живёт в этом же процессе, сотрудники вымышленные, пароль у всех один. В выпускной пакет не входит.
//!
//! Нужна пустая база (`PULT_DATABASE_URL`). При каждом запуске домен в базе перенастраивается на новый
//! адрес и CA подделки; данные портала (заявки, настройки) сохраняются между запусками.

use crate::{config::Config, say};
use anyhow::Context;
use pult_core::Secret;
use pult_testkit::{BASE_DN, Directory, FakeDc, FakeUser, SVC_DN, SVC_PASS, TestCa, USER_FILTER, USER_PASS};

/// Вымышленные сотрудники: логин, фамилия, имя.
const PEOPLE: &[(&str, &str, &str)] = &[
    ("admin", "Соколов", "Андрей"),
    ("engineer", "Кузнецова", "Мария"),
    ("engineer2", "Лебедев", "Павел"),
    ("ivanov", "Иванов", "Иван"),
    ("petrova", "Петрова", "Анна"),
    ("smirnov", "Смирнов", "Дмитрий"),
    ("volkova", "Волкова", "Елена"),
    ("morozov", "Морозов", "Сергей"),
    ("novikova", "Новикова", "Ольга"),
    ("fedorov", "Фёдоров", "Николай"),
];

pub async fn run(mut cfg: Config) -> anyhow::Result<()> {
    let users =
        PEOPLE.iter().zip(1u8..).map(|((sam, sn, given), seed)| FakeUser::person(sam, sn, given, seed)).collect();
    let dir = Directory::new(users);
    dir.add_group("Бухгалтерия — общая папка", &["petrova"]);
    let ca = TestCa::new("Pult Demo CA");
    let dc = FakeDc::start(&ca, dir).await;

    let pool = crate::db_pool(&cfg, false).await?;
    crate::migrate::run(&pool, &sqlx::migrate!("../../migrations")).await?;
    let core = crate::core_state(&cfg, pool.clone())?;
    let secret = core.secrets.encrypt(&Secret::new(SVC_PASS)).context("шифрование пароля службы")?;
    sqlx::query(
        "INSERT INTO ad_domains (dns_name, base_dn, controllers, tls_mode, bind_dn, bind_secret, ca_cert_pem, user_filter, timeout_secs)
         VALUES ('pult.test', $1, $2, 'ldaps', $3, $4, $5, $6, 5)
         ON CONFLICT (dns_name) DO UPDATE
            SET controllers = EXCLUDED.controllers, bind_secret = EXCLUDED.bind_secret, ca_cert_pem = EXCLUDED.ca_cert_pem",
    )
    .bind(BASE_DN)
    .bind(vec![dc.endpoint()])
    .bind(SVC_DN)
    .bind(secret)
    .bind(ca.pem())
    .bind(USER_FILTER)
    .execute(&pool)
    .await
    .context("домен демо")?;
    pool.close().await;

    cfg.initial_superadmin = Some("admin".into());
    say("Демо Pult: домен pult.test (поддельный), сотрудники вымышленные.");
    say(&format!("Вход: admin (суперадминистратор), engineer, ivanov, petrova… — пароль {USER_PASS}"));
    let r = crate::serve(cfg).await;
    dc.stop().await;
    r
}
