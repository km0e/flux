CREATE TABLE IF NOT EXISTS buf_entries (
    chat_id  TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    call_id  TEXT NOT NULL,
    content  TEXT NOT NULL,
    PRIMARY KEY (chat_id, call_id)
);
