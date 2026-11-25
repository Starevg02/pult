-- Каталог услуг: короткое описание категории для сотрудника («что сюда относится»).
ALTER TABLE ticket_categories ADD COLUMN description text NOT NULL DEFAULT '';
