-- Chaff catalog, schema version 5: the measurements behind a score.
--
-- ## Why this exists
--
-- Scoring a photograph means decoding it and running eight metrics over the pixels. On a
-- real library that is the whole cost of an index pass — the walk is milliseconds and the
-- decoding is minutes. Measured on 400 real photographs: **8.4 s first time, 8.2 s on a
-- re-run.** Nothing was reused, because nothing was kept.
--
-- The composite cannot be reused directly, because shoot-relative ranking is a property of
-- the whole set: adding one photograph to a shoot changes every percentile in it. But the
-- *measurements* are per-photograph and depend on nothing else. Keep them, and a re-run
-- re-ranks in microseconds instead of re-decoding for minutes.
--
-- ## What invalidates a row
--
-- `measured_size` and `measured_mtime` record the file the measurement came from. On the
-- next pass they are compared against the `file` row — one `stat`, no decode — and any
-- difference means the pixels may have changed, so it is measured again.
--
-- Deliberately *not* the content hash. That is filled lazily and costs a full read of a
-- 45 MB raw; using it here would trade minutes of decoding for minutes of hashing. Size and
-- modification time are what every build system uses for the same reason.
--
-- ## It is keyed by scorer version
--
-- A measurement is only meaningful to the scorer that produced it. Changing a metric and
-- re-running must not silently reuse numbers computed by the old one.

CREATE TABLE measurement (
    photo_id       INTEGER NOT NULL REFERENCES photo (id) ON DELETE CASCADE,
    scorer_version INTEGER NOT NULL,
    -- The file this was measured from, so a changed file is detected without reading it.
    measured_path  TEXT    NOT NULL,
    measured_size  INTEGER NOT NULL,
    measured_mtime INTEGER NOT NULL,
    -- Grouping inputs, which the metric values do not carry.
    camera         TEXT,
    captured_at    INTEGER,
    -- The eight metric values, by their `Metric` discriminant. Stored as columns rather
    -- than rows: they are always read together, always written together, and a fixed set
    -- is simpler to keep correct than a key-value table with eight guaranteed keys.
    m0 REAL NOT NULL, m1 REAL NOT NULL, m2 REAL NOT NULL, m3 REAL NOT NULL,
    m4 REAL NOT NULL, m5 REAL NOT NULL, m6 REAL NOT NULL, m7 REAL NOT NULL,
    measured_at    INTEGER NOT NULL,
    PRIMARY KEY (photo_id, scorer_version)
);

-- The pass reads every measurement for a version at once, to decide what to skip.
CREATE INDEX measurement_version ON measurement (scorer_version);
