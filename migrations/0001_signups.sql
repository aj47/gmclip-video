CREATE TABLE signups (
  email TEXT PRIMARY KEY,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  consent_version TEXT NOT NULL
);
CREATE TABLE signup_limits (
  key TEXT PRIMARY KEY,
  hour INTEGER NOT NULL,
  count INTEGER NOT NULL
);
CREATE INDEX signup_limits_hour ON signup_limits(hour);
