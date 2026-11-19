//! Перебор контроллеров домена.
//!
//! Правило: при ошибке соединения — следующий DC из списка; рабочий DC
//! запоминается и используется, пока отвечает.
//! Ошибка, которую вернул сам контроллер (неверный пароль, нет прав), **не** приводит
//! к повтору на другом DC.

use std::future::Future;

/// Результат одной попытки на конкретном контроллере.
pub enum Attempt<T, E> {
    /// Контроллер ответил — успех или содержательный отказ. Перебор останавливается.
    Answered(Result<T, E>),
    /// Контроллер недоступен (сеть, TLS, таймаут) — пробуем следующий.
    Unreachable(String),
}

/// Обходит контроллеры, начиная с `preferred`, и возвращает индекс ответившего.
/// Если не ответил никто — список причин по каждому DC.
pub async fn try_in_order<D, T, E, F, Fut>(
    controllers: &[D],
    preferred: usize,
    mut attempt: F,
) -> Result<(usize, Result<T, E>), Vec<(usize, String)>>
where
    F: FnMut(usize, &D) -> Fut,
    Fut: Future<Output = Attempt<T, E>>,
{
    let n = controllers.len();
    let mut failures = Vec::new();
    for offset in 0..n {
        let i = (preferred + offset) % n;
        match attempt(i, &controllers[i]).await {
            Attempt::Answered(r) => return Ok((i, r)),
            Attempt::Unreachable(why) => failures.push((i, why)),
        }
    }
    Err(failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[tokio::test]
    async fn first_dc_down_switches_to_second() {
        let dcs = ["dc1", "dc2"];
        let tried = RefCell::new(Vec::new());
        let r = try_in_order(&dcs, 0, |_, dc| {
            tried.borrow_mut().push(*dc);
            let dc = *dc;
            async move {
                if dc == "dc1" {
                    Attempt::Unreachable("connection refused".into())
                } else {
                    Attempt::Answered(Ok::<_, ()>(dc))
                }
            }
        })
        .await;
        assert_eq!(r.ok().map(|(i, v)| (i, v.ok())), Some((1, Some("dc2"))));
        assert_eq!(*tried.borrow(), vec!["dc1", "dc2"]);
    }

    #[tokio::test]
    async fn starts_from_remembered_dc() {
        let dcs = ["dc1", "dc2", "dc3"];
        let tried = RefCell::new(Vec::new());
        let _ = try_in_order(&dcs, 2, |_, dc| {
            tried.borrow_mut().push(*dc);
            async { Attempt::<(), ()>::Unreachable("down".into()) }
        })
        .await;
        assert_eq!(*tried.borrow(), vec!["dc3", "dc1", "dc2"]);
    }

    #[tokio::test]
    async fn wrong_password_is_not_retried_on_other_dc() {
        let dcs = ["dc1", "dc2"];
        let tried = RefCell::new(0);
        let r = try_in_order(&dcs, 0, |_, _| {
            *tried.borrow_mut() += 1;
            async { Attempt::<(), &str>::Answered(Err("invalid credentials")) }
        })
        .await;
        assert_eq!(*tried.borrow(), 1);
        assert!(matches!(r, Ok((0, Err("invalid credentials")))));
    }

    #[tokio::test]
    async fn all_down_reports_each_dc() {
        let dcs = ["dc1", "dc2"];
        let r = try_in_order(&dcs, 0, |i, _| async move { Attempt::<(), ()>::Unreachable(format!("down {i}")) }).await;
        assert_eq!(r.err(), Some(vec![(0, "down 0".to_string()), (1, "down 1".to_string())]));
    }
}
