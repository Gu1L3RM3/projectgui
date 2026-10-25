CREATE TABLE project (
    id           TEXT PRIMARY KEY,
    slug         TEXT NOT NULL UNIQUE,
    title        TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    priority     TEXT NOT NULL CHECK (priority IN ('low','medium','high','critical')),
    stage        TEXT NOT NULL CHECK (stage IN ('idea','planning','in-progress','review','done','paused')),
    path         TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    archived_at  TEXT
);

CREATE TABLE project_slug_alias (
    slug        TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE
);

CREATE TABLE task (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    description   TEXT,
    status        TEXT NOT NULL CHECK (status IN ('todo','in-progress','blocked','done')),
    priority      TEXT CHECK (priority IS NULL OR priority IN ('low','medium','high','critical')),
    position      INTEGER NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    completed_at  TEXT
);
CREATE INDEX task_project ON task(project_id, position);

CREATE TABLE reference (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    task_id     TEXT REFERENCES task(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('commit','pr','issue')),
    value       TEXT NOT NULL,
    url         TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX reference_project ON reference(project_id);

CREATE TABLE kv (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
