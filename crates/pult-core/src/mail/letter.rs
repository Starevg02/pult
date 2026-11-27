//! Оформление HTML-писем: карточка на светлом фоне — цветная полоса по смыслу письма, надзаголовок
//! (организация, номер заявки), заголовок, текст в рамке, кнопки и подвал. Вёрстка таблицами и
//! встроенными стилями: так письмо одинаково выглядит в Outlook, веб-почте и на телефоне.
//! Весь текст экранируется здесь — вызывающий передаёт обычные строки.

/// Цвет кнопки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Основное действие (цвет письма).
    Primary,
    /// «Да» — зелёная.
    Good,
    /// «Нет» — контурная красная.
    Bad,
}

/// Письмо: что показать.
#[derive(Debug, Clone, Default)]
pub struct Letter<'a> {
    /// Язык письма (`ru`, `en`, …); пусто — `ru`.
    pub lang: &'a str,
    /// Организация (над заголовком и в подвале).
    pub brand: &'a str,
    /// Надзаголовок рядом с организацией: «Заявка #1042».
    pub eyebrow: &'a str,
    /// Заголовок письма.
    pub title: &'a str,
    /// Строка под заголовком: тема заявки.
    pub subtitle: &'a str,
    /// Цвет письма (#rrggbb): полоса сверху, рамка текста, основная кнопка.
    pub accent: &'a str,
    /// Текст: переносы строк сохраняются.
    pub body: &'a str,
    /// Вопрос перед кнопками («Проблема решена?»).
    pub question: &'a str,
    /// Кнопки: адрес, надпись, вид.
    pub buttons: Vec<(String, String, Button)>,
    /// Мелкий текст в подвале.
    pub footer: &'a str,
    /// Адрес портала для ссылки в подвале.
    pub portal: &'a str,
}

/// Экранирование для HTML (текст и значения атрибутов).
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Цвет вида `#abc` или `#aabbcc`; иначе — цвет портала.
fn color(c: &str) -> &str {
    let ok = c.starts_with('#') && matches!(c.len(), 4 | 7) && c[1..].chars().all(|x| x.is_ascii_hexdigit());
    if ok { c } else { "#4f46e5" }
}

const FONT: &str = "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,'Helvetica Neue',Arial,sans-serif";

/// HTML письма.
pub fn render(l: &Letter<'_>) -> String {
    let accent = color(l.accent);
    let brand = if l.brand.trim().is_empty() { "Pult" } else { l.brand.trim() };
    // Первая строка текста — превью в списке писем (скрыта в самом письме).
    let preheader: String =
        l.body.lines().find(|x| !x.trim().is_empty()).unwrap_or(l.title).chars().take(140).collect();
    let mut h = String::with_capacity(4096);
    let lang =
        if !l.lang.is_empty() && l.lang.len() <= 8 && l.lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            l.lang
        } else {
            "ru"
        };
    h.push_str(&format!("<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\">"));
    h.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    h.push_str(
        "<meta name=\"color-scheme\" content=\"light\"><meta name=\"supported-color-schemes\" content=\"light\">",
    );
    h.push_str(&format!("<title>{}</title></head>", esc(l.title)));
    h.push_str(&format!(
        "<body style=\"margin:0;padding:0;background:#eef1f6;-webkit-text-size-adjust:100%\">\
         <div style=\"display:none;max-height:0;overflow:hidden;opacity:0;color:transparent\">{}</div>",
        esc(&preheader)
    ));
    h.push_str(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         style=\"background:#eef1f6;padding:28px 12px\"><tr><td align=\"center\">",
    );
    h.push_str(&format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         style=\"max-width:580px;background:#ffffff;border:1px solid #e4e7ec;border-radius:16px;overflow:hidden;\
         font-family:{FONT};color:#16181d\">"
    ));
    // Полоса цвета письма.
    h.push_str(&format!(
        "<tr><td style=\"height:5px;line-height:5px;font-size:0;background:{accent}\">&nbsp;</td></tr>"
    ));
    // Шапка: организация · надзаголовок, заголовок, тема.
    h.push_str("<tr><td style=\"padding:24px 32px 4px\">");
    h.push_str(&format!(
        "<div style=\"font-size:12px;line-height:16px;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:#7a8191\">\
         <span style=\"color:{accent}\">&#9679;</span>&nbsp; {}{}</div>",
        esc(brand),
        if l.eyebrow.is_empty() { String::new() } else { format!(" &middot; {}", esc(l.eyebrow)) }
    ));
    h.push_str(&format!(
        "<h1 style=\"margin:10px 0 0;font-size:22px;line-height:28px;font-weight:700;color:#16181d\">{}</h1>",
        esc(l.title)
    ));
    if !l.subtitle.trim().is_empty() {
        h.push_str(&format!(
            "<div style=\"margin-top:4px;font-size:15px;line-height:21px;color:#596070\">{}</div>",
            esc(l.subtitle)
        ));
    }
    h.push_str("</td></tr>");
    // Текст.
    if !l.body.trim().is_empty() {
        h.push_str(&format!(
            "<tr><td style=\"padding:18px 32px 4px\"><div style=\"background:#f6f7fb;border-left:4px solid {accent};\
             border-radius:10px;padding:16px 18px;font-size:15px;line-height:23px;color:#16181d;white-space:pre-wrap;\
             word-break:break-word\">{}</div></td></tr>",
            esc(l.body)
        ));
    }
    // Вопрос и кнопки.
    if !l.question.is_empty() || !l.buttons.is_empty() {
        h.push_str("<tr><td style=\"padding:18px 32px 6px\">");
        if !l.question.is_empty() {
            h.push_str(&format!(
                "<div style=\"margin-bottom:12px;font-size:15px;line-height:21px;font-weight:600\">{}</div>",
                esc(l.question)
            ));
        }
        for (url, text, kind) in &l.buttons {
            let style = match kind {
                Button::Primary => format!("background:{accent};color:#ffffff;border:2px solid {accent}"),
                Button::Good => "background:#16a34a;color:#ffffff;border:2px solid #16a34a".to_string(),
                Button::Bad => "background:#ffffff;color:#b91c1c;border:2px solid #f1b4b4".to_string(),
            };
            h.push_str(&format!(
                "<a href=\"{}\" style=\"display:inline-block;margin:0 8px 8px 0;padding:11px 22px;border-radius:10px;\
                 font-size:15px;line-height:20px;font-weight:600;text-decoration:none;{style}\">{}</a>",
                esc(url),
                esc(text)
            ));
        }
        h.push_str("</td></tr>");
    }
    // Подвал.
    h.push_str("<tr><td style=\"padding:18px 32px 0\"></td></tr>");
    h.push_str(
        "<tr><td style=\"padding:16px 32px 20px;background:#f8f9fb;border-top:1px solid #eef0f3;font-size:12px;\
         line-height:18px;color:#7a8191\">",
    );
    if !l.footer.is_empty() {
        h.push_str(&format!("<div>{}</div>", esc(l.footer)));
    }
    let site = if l.portal.is_empty() {
        esc(brand)
    } else {
        format!("<a href=\"{}\" style=\"color:#596070;text-decoration:underline\">{}</a>", esc(l.portal), esc(brand))
    };
    h.push_str(&format!("<div style=\"margin-top:6px;color:#9aa1ad\">{site}</div>"));
    h.push_str("</td></tr></table></td></tr></table></body></html>");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_escapes_everything_and_keeps_layout() {
        let h = render(&Letter {
            lang: "en\"><script>",
            brand: "ООО <Ромашка>",
            eyebrow: "Заявка #7",
            title: "Ответ \"по\" заявке",
            subtitle: "<b>Принтер</b>",
            accent: "red;background:url(x)",
            body: "Строка 1\n<script>alert(1)</script>",
            question: "Решена?",
            buttons: vec![("https://p/x?a=1&b=\"2\"".into(), "Да".into(), Button::Good)],
            footer: "Отвечать не нужно",
            portal: "https://p",
        });
        assert!(!h.contains("<script>") && h.contains("&lt;script&gt;"));
        assert!(!h.contains("<b>Принтер") && h.contains("&lt;b&gt;Принтер"));
        assert!(h.contains("ООО &lt;Ромашка&gt;"));
        assert!(h.contains("a=1&amp;b=&quot;2&quot;"), "адрес кнопки экранирован");
        assert!(!h.contains("url(x)") && h.contains("#4f46e5"), "чужой цвет не попадает в стиль");
        assert!(h.contains("Строка 1"), "превью письма — первая строка текста");
        assert!(h.contains("<html lang=\"ru\">"), "чужой язык не попадает в разметку");
    }

    #[test]
    fn empty_brand_and_parts_are_skipped() {
        let h = render(&Letter { title: "Заголовок", accent: "#16a34a", ..Default::default() });
        assert!(h.contains(">Pult") && h.contains("#16a34a"));
        assert!(!h.contains("border-left:4px"), "без текста нет рамки");
    }
}
