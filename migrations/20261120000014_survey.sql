-- Подробный опрос после решения: необязательные баллы 1–5 к оценке «хорошо / плохо».
ALTER TABLE ticket_ratings
    ADD COLUMN speed    smallint NULL CHECK (speed BETWEEN 1 AND 5),
    ADD COLUMN quality  smallint NULL CHECK (quality BETWEEN 1 AND 5),
    ADD COLUMN courtesy smallint NULL CHECK (courtesy BETWEEN 1 AND 5);
