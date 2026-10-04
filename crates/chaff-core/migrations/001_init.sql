-- Chaff catalog, schema version 1.
--
-- Design notes that matter:
--
-- * Timestamps are Unix epoch **seconds** as INTEGER, not TEXT. They sort, compare and
--   index correctly, and there is no timezone or format ambiguity to get wrong. The UI
--   converts for display; storage stays unambiguous.
--
-- * `photo` is the culling unit, not `file`. One photograph is one RAW plus one JPEG
--   plus sidecars, and every user-facing action — rate, reject, band, delete — applies
--   to the photograph. Keeping `file` separate is what makes the paired delete of
--   ADR-0004 expressible as a single database operation rather than a heuristic.
--
-- * `photo.stem` is stored NFC-normalised and lowercased, exactly as `pair.rs` compares
--   it. Storing the raw filename here would reintroduce the macOS/Linux divergence that
--   `pair.rs` exists to remove.
--
-- * `score` is keyed by `scorer_version`. Re-scoring after a metric changes writes new
--   rows rather than overwriting old ones, so "why did this photograph's score change?"
--   is answerable, and a scorer regression can be rolled back by reverting the version.
--
-- * Nothing here records anything the user did not ask for. There is no telemetry table,
--   no device identifier, and no column that leaves this machine.

CREATE TABLE library (
    id              INTEGER PRIMARY KEY,
    root            TEXT    NOT NULL UNIQUE,
    created_at      INTEGER NOT NULL,
    last_indexed_at INTEGER
);

CREATE TABLE photo (
    id           INTEGER PRIMARY KEY,
    library_id   INTEGER NOT NULL REFERENCES library (id) ON DELETE CASCADE,
    dir          TEXT    NOT NULL,
    stem         TEXT    NOT NULL,
    state        TEXT    NOT NULL
                 CHECK (state IN ('pair', 'raw_only', 'raster_only', 'ambiguous')),
    needs_review INTEGER NOT NULL DEFAULT 0
                 CHECK (needs_review IN (0, 1)),
    UNIQUE (library_id, dir, stem)
);

CREATE INDEX photo_library ON photo (library_id);
-- Named `photo_needs_review`, not `photo_review`: SQLite index names share a namespace
-- with table names, and `photo_review` is a table. The collision fails at migration time
-- with "there is already an index named photo_review".
CREATE INDEX photo_needs_review ON photo (needs_review) WHERE needs_review = 1;

CREATE TABLE file (
    id           INTEGER PRIMARY KEY,
    library_id   INTEGER NOT NULL REFERENCES library (id) ON DELETE CASCADE,
    photo_id     INTEGER REFERENCES photo (id) ON DELETE CASCADE,
    path         TEXT    NOT NULL,
    role         TEXT    NOT NULL
                 CHECK (role IN ('raw', 'raster', 'sidecar', 'video')),
    size_bytes   INTEGER NOT NULL,
    mtime_ns     INTEGER NOT NULL,
    -- Filled lazily: hashing every file during a first index would double the wall-clock
    -- cost of the one operation users are least patient with. The paired delete computes
    -- it on demand and compares, which is the only place it is load-bearing.
    content_hash TEXT,
    indexed_at   INTEGER NOT NULL,
    UNIQUE (library_id, path)
);

CREATE INDEX file_photo ON file (photo_id);
CREATE INDEX file_role  ON file (library_id, role);

CREATE TABLE photo_review (
    photo_id INTEGER NOT NULL REFERENCES photo (id) ON DELETE CASCADE,
    reason   TEXT    NOT NULL,
    PRIMARY KEY (photo_id, reason)
);

CREATE TABLE score (
    photo_id       INTEGER NOT NULL REFERENCES photo (id) ON DELETE CASCADE,
    metric         TEXT    NOT NULL,
    value          REAL    NOT NULL,
    scorer_version INTEGER NOT NULL,
    computed_at    INTEGER NOT NULL,
    PRIMARY KEY (photo_id, metric, scorer_version)
);

CREATE INDEX score_metric ON score (metric, scorer_version);
