//! Политика подключения (перенос `policy.json` RUSSIANANYDESK).
//!
//! Решает только сервер — helper получает готовые параметры и ослабить их не может:
//! - запрещённый ПК — подключиться нельзя совсем (никаким действием);
//! - ПК с обязательным согласием — пользователь всегда видит запрос, даже у оператора с правом управления;
//! - `remote.connect.view` — только просмотр и всегда с согласием (бывшая роль Viewer);
//! - `remote.connect.control` — управление, согласие по выбору оператора (бывшая роль Full);
//! - быстрые действия с доступом к ПК (`C$`, «Управление компьютером», PowerShell) — как управление,
//!   проверка доступности — как просмотр.

use pult_api_types::{RemoteAction, ShadowMode};

/// Правило из `remote_policies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Deny,
    ConsentRequired,
}

impl Rule {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "deny" => Some(Self::Deny),
            "consent_required" => Some(Self::ConsentRequired),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::ConsentRequired => "consent_required",
        }
    }
}

/// Права оператора на подключение.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rights {
    pub view: bool,
    pub control: bool,
}

/// Запрос согласия у пользователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Prompted,
    Skipped,
}

impl Consent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompted => "prompted",
            Self::Skipped => "skipped",
        }
    }
}

/// Почему отказано.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Нет нужного разрешения.
    NoPermission,
    /// ПК в списке запрещённых.
    DeviceDenied,
    /// ПК «только с согласием»: доступ к файлам, управлению и PowerShell идёт без ведома
    /// пользователя, поэтому разрешено только теневое подключение с запросом и проверка доступности.
    ConsentOnly,
}

/// Что хочет оператор.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ask {
    pub action: RemoteAction,
    /// Для `shadow`; без режима — просмотр.
    pub mode: Option<ShadowMode>,
    /// «Без запроса согласия» (только управление и только если политика ПК позволяет).
    pub skip_consent: bool,
}

/// Решение: для `shadow` — режим и согласие, для остальных действий — `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowed {
    pub mode: Option<ShadowMode>,
    pub consent: Option<Consent>,
}

/// Совпадает ли имя ПК (короткое или FQDN) с шаблоном. Без учёта регистра, `*` — любые символы.
pub fn matches(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) =
        (pattern.trim().to_lowercase().chars().collect(), name.trim().to_lowercase().chars().collect());
    if p.is_empty() || n.is_empty() {
        return false;
    }
    // Классический жадный разбор с откатом к последней `*`: O(len(p) * len(n)) в худшем случае.
    let (mut i, mut j, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while j < n.len() {
        if i < p.len() && p[i] != '*' && p[i] == n[j] {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

/// Правила, применимые к ПК: совпадение с коротким именем или FQDN.
pub fn rules_for<'a>(
    policies: impl IntoIterator<Item = (&'a str, Rule)>,
    hostname: &str,
    fqdn: Option<&str>,
) -> Vec<Rule> {
    let mut out: Vec<Rule> = policies
        .into_iter()
        .filter(|(p, _)| matches(p, hostname) || fqdn.is_some_and(|f| matches(p, f)))
        .map(|(_, r)| r)
        .collect();
    out.dedup();
    out
}

/// Имя, по которому helper подключается к ПК: имя компьютера из AD (проверено при регистрации)
/// и DNS-имя его домена из настроек Pult. Ни имя от агента, ни `dNSHostName` компьютера не
/// используются: их может подменить владелец ПК и увести учётные данные инженера на свой узел.
pub fn connect_target(hostname: &str, domain: Option<&str>) -> String {
    let host = hostname.trim().to_ascii_lowercase();
    match domain.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => format!("{host}.{}", d.to_ascii_lowercase()),
        None => host,
    }
}

/// Решение по запросу оператора.
pub fn decide(rules: &[Rule], rights: Rights, ask: Ask) -> Result<Allowed, Refusal> {
    if !rights.view && !rights.control {
        return Err(Refusal::NoPermission);
    }
    if rules.contains(&Rule::Deny) {
        return Err(Refusal::DeviceDenied);
    }
    match ask.action {
        RemoteAction::Shadow => {
            let mode = ask.mode.unwrap_or(ShadowMode::View);
            match mode {
                ShadowMode::View => Ok(Allowed { mode: Some(mode), consent: Some(Consent::Prompted) }),
                ShadowMode::Control if !rights.control => Err(Refusal::NoPermission),
                ShadowMode::Control => {
                    let forced = rules.contains(&Rule::ConsentRequired);
                    let consent = if ask.skip_consent && !forced { Consent::Skipped } else { Consent::Prompted };
                    Ok(Allowed { mode: Some(mode), consent: Some(consent) })
                }
            }
        }
        RemoteAction::Files | RemoteAction::Manage | RemoteAction::Powershell if !rights.control => {
            Err(Refusal::NoPermission)
        }
        RemoteAction::Files | RemoteAction::Manage | RemoteAction::Powershell
            if rules.contains(&Rule::ConsentRequired) =>
        {
            Err(Refusal::ConsentOnly)
        }
        RemoteAction::Files | RemoteAction::Manage | RemoteAction::Powershell | RemoteAction::Probe => {
            Ok(Allowed { mode: None, consent: None })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: Rights = Rights { view: true, control: false };
    const CONTROL: Rights = Rights { view: false, control: true };
    const BOTH: Rights = Rights { view: true, control: true };

    fn shadow(mode: ShadowMode, skip: bool) -> Ask {
        Ask { action: RemoteAction::Shadow, mode: Some(mode), skip_consent: skip }
    }

    #[test]
    fn patterns() {
        assert!(matches("Kassa-Pc", "KASSA-PC"));
        assert!(matches("ws-*", "WS-001"));
        assert!(matches("*.buh.example.local", "pc1.buh.example.local"));
        assert!(matches("*", "anything"));
        assert!(matches("a*b*c", "aXXbYYc"));
        assert!(!matches("ws-*", "srv-ws-1"));
        assert!(!matches("ws", "ws-1"), "без звёздочки — только целиком");
        assert!(!matches("", "ws"));
        assert!(!matches("a*b", "acbd"));
        // Длинный шаблон из звёздочек не зависает.
        assert!(!matches(&"*a".repeat(100), &"a".repeat(99)));
    }

    #[test]
    fn rules_by_hostname_or_fqdn() {
        let p = [("buh-pc3", Rule::ConsentRequired), ("*.secret.local", Rule::Deny), ("other", Rule::Deny)];
        assert_eq!(rules_for(p, "BUH-PC3", None), vec![Rule::ConsentRequired]);
        assert_eq!(rules_for(p, "PC1", Some("pc1.secret.local")), vec![Rule::Deny]);
        assert!(rules_for(p, "PC2", Some("pc2.example.local")).is_empty());
    }

    #[test]
    fn view_always_prompts() {
        for rights in [VIEW, CONTROL, BOTH] {
            let a = decide(&[], rights, shadow(ShadowMode::View, true)).unwrap();
            assert_eq!(a.consent, Some(Consent::Prompted), "просмотр — всегда с согласием");
            assert_eq!(a.mode, Some(ShadowMode::View));
        }
    }

    #[test]
    fn control_needs_control_right() {
        assert_eq!(decide(&[], VIEW, shadow(ShadowMode::Control, false)), Err(Refusal::NoPermission));
        assert_eq!(decide(&[], Rights::default(), shadow(ShadowMode::View, false)), Err(Refusal::NoPermission));
        let a = decide(&[], CONTROL, shadow(ShadowMode::Control, true)).unwrap();
        assert_eq!(a.consent, Some(Consent::Skipped));
        let a = decide(&[], CONTROL, shadow(ShadowMode::Control, false)).unwrap();
        assert_eq!(a.consent, Some(Consent::Prompted));
    }

    #[test]
    fn consent_required_device_cannot_skip() {
        let a = decide(&[Rule::ConsentRequired], BOTH, shadow(ShadowMode::Control, true)).unwrap();
        assert_eq!(a.consent, Some(Consent::Prompted), "ПК из списка — согласие даже с правом управления");
        assert_eq!(a.mode, Some(ShadowMode::Control));
    }

    #[test]
    fn denied_device_refuses_everything() {
        for action in [
            RemoteAction::Shadow,
            RemoteAction::Files,
            RemoteAction::Manage,
            RemoteAction::Powershell,
            RemoteAction::Probe,
        ] {
            let ask = Ask { action, mode: None, skip_consent: false };
            assert_eq!(
                decide(&[Rule::Deny, Rule::ConsentRequired], BOTH, ask),
                Err(Refusal::DeviceDenied),
                "{action:?}"
            );
        }
    }

    #[test]
    fn quick_actions() {
        let ask = |action| Ask { action, mode: None, skip_consent: false };
        assert_eq!(decide(&[], VIEW, ask(RemoteAction::Files)), Err(Refusal::NoPermission));
        assert_eq!(decide(&[], VIEW, ask(RemoteAction::Powershell)), Err(Refusal::NoPermission));
        assert!(decide(&[], VIEW, ask(RemoteAction::Probe)).is_ok());
        let a = decide(&[], CONTROL, ask(RemoteAction::Manage)).unwrap();
        assert_eq!(a, Allowed { mode: None, consent: None });
    }

    #[test]
    fn consent_required_device_allows_only_shadow_with_prompt_and_probe() {
        let ask = |action| Ask { action, mode: None, skip_consent: false };
        for a in [RemoteAction::Files, RemoteAction::Manage, RemoteAction::Powershell] {
            assert_eq!(decide(&[Rule::ConsentRequired], BOTH, ask(a)), Err(Refusal::ConsentOnly), "{a:?}");
        }
        assert!(decide(&[Rule::ConsentRequired], BOTH, ask(RemoteAction::Probe)).is_ok());
    }

    #[test]
    fn connect_target_comes_from_ad_name_and_domain_only() {
        assert_eq!(connect_target("WS-EMP", Some("Pult.Test")), "ws-emp.pult.test");
        assert_eq!(connect_target("WS-EMP", None), "ws-emp");
        assert_eq!(connect_target("WS-EMP", Some("")), "ws-emp");
    }
}
