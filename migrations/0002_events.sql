-- Which links and sites bring people to gmclip.com (functions/api/e.js, POST /api/e): one row
-- when a visit starts, when someone clicks a download or GitHub link, and when someone joins
-- the updates list, each with the link's utm_* tags or the referring site. Nothing that
-- identifies a person: no IP address, device or visitor id, or email.
CREATE TABLE events (
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  day TEXT NOT NULL,
  name TEXT NOT NULL,
  path TEXT,
  ref TEXT,
  source TEXT,
  medium TEXT,
  campaign TEXT,
  content TEXT,
  term TEXT,
  detail TEXT
);
CREATE INDEX events_day ON events(day, name);
