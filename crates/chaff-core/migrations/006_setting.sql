-- Chaff catalog, schema version 6: small remembered values.
--
-- ## Why the catalog and not the frontend
--
-- A window's `localStorage` is per-webview and can be cleared by anything that clears site
-- data; the catalog is the application's own durable state and is already backed up before
-- every schema upgrade. "Which library was I working in?" is not a browser preference.
--
-- ## Why a key-value table and not columns
--
-- These are few, heterogeneous, and added one at a time — a column per preference means a
-- migration per preference. The values are opaque strings; the caller that writes a key
-- owns its format, and nothing here interprets them.
--
-- Deliberately **not** a place for anything the engine depends on. If losing a value would
-- change what a photograph scores, it belongs in a real table with a real schema.

CREATE TABLE setting (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
