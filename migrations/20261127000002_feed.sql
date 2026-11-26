-- Лента записей со всех стен (новые сверху) и суточная квота вложений сообщений по сотруднику.
CREATE INDEX wall_posts_feed ON wall_posts (created_at DESC, id DESC) WHERE parent_id IS NULL;
CREATE INDEX chat_files_uploader ON chat_files (uploaded_by, created_at);
