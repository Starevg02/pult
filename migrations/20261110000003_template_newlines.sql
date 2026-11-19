-- Описания шаблонов заявок из 20261110000002 были записаны обычными строками, и «\n» сохранился
-- буквально (обратная косая черта и n). Заменяем на настоящий перевод строки.
UPDATE ticket_templates
   SET description = replace(description, '\n', E'\n'), updated_at = now()
 WHERE strpos(description, '\n') > 0;
