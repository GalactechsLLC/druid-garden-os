ALTER TABLE users ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX users_username_unique ON users(username);
