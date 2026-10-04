-- Chaff catalog, schema version 2: EXIF.
--
-- Separate table rather than columns on `file`, for two reasons:
--
--  * `file` is written on every index pass, for every file. EXIF is read once per file
--    and rarely changes, so widening the hot table would slow the common path to serve
--    the rare one.
--  * A file with no EXIF is a normal, common case (stripped exports, screenshots,
--    scans). Keeping it in its own table keeps `file` free of eight mostly-null columns.
--
-- `source_mtime_ns` is the load-bearing column. The indexer re-reads EXIF only when a
-- file's modification time differs from the one recorded here, so a re-index of an
-- unchanged library does no EXIF I/O at all. It also means a row with all-NULL fields
-- is meaningful: it records "this file was examined and holds no EXIF", which stops the
-- reader being run against the same metadata-free file on every pass forever.
--
-- `captured_at` carries the timezone caveat documented in `exif.rs`: EXIF stores local
-- wall-clock time with no offset, so only *differences* between values are meaningful.
-- It is indexed because burst grouping orders and windows on it.

CREATE TABLE exif (
    file_id         INTEGER PRIMARY KEY REFERENCES file (id) ON DELETE CASCADE,
    source_mtime_ns INTEGER NOT NULL,
    captured_at     INTEGER,
    make            TEXT,
    model           TEXT,
    lens            TEXT,
    iso             INTEGER,
    f_number        REAL,
    exposure_time   REAL,
    focal_length    REAL,
    orientation     INTEGER,
    read_at         INTEGER NOT NULL
);

CREATE INDEX exif_captured ON exif (captured_at) WHERE captured_at IS NOT NULL;
CREATE INDEX exif_model    ON exif (model)      WHERE model IS NOT NULL;
