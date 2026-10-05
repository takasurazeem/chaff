-- Chaff catalog, schema version 7: faces found in a photograph.
--
-- ## Faces are not people
--
-- A row here says a face was *detected* — a rectangle, five landmarks and a confidence. It
-- says nothing about who. Identity is a separate table (#45) populated by clustering
-- embeddings and confirmed by a person, and keeping the two apart in the schema is what
-- stops a name from being attached to a face by accident.
--
-- ## Why the boxes and not just a count
--
-- A count answers "does this photograph have people in it", which is the filter most people
-- want. The boxes are what a *crop* needs, and re-running detection to get them back would
-- mean re-running a model over the whole library because a number was not stored.
--
-- ## Keyed by the file it was detected in
--
-- The same photograph may have a raw and a JPEG, and detection runs on whichever the
-- thumbnail pipeline could read. Recording which file it was means a changed file
-- invalidates its faces the same way it invalidates a measurement.

CREATE TABLE face (
    id           INTEGER PRIMARY KEY,
    file_id      INTEGER NOT NULL REFERENCES file (id) ON DELETE CASCADE,
    -- Pixel coordinates in the image the detector saw.
    x            REAL NOT NULL,
    y            REAL NOT NULL,
    width        REAL NOT NULL,
    height       REAL NOT NULL,
    confidence   REAL NOT NULL,
    -- The five landmarks, flat: re_x, re_y, le_x, le_y, nt_x, nt_y, rcm_x, rcm_y, lcm_x, lcm_y.
    -- Stored because they are free — the model emits them with the box — and because
    -- alignment for an embedding needs them.
    landmarks    BLOB NOT NULL,
    -- The file identity at detection time, so a changed file is detected without reading it.
    detected_size  INTEGER NOT NULL,
    detected_mtime INTEGER NOT NULL,
    detector     TEXT NOT NULL,
    detected_at  INTEGER NOT NULL
);

CREATE INDEX face_file ON face (file_id);

-- The grid asks "which photographs have faces" for a filter. Without this it scans.
CREATE INDEX face_confidence ON face (confidence);
