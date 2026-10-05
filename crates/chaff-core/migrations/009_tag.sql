-- Chaff catalog, schema version 9: tags.
--
-- ## A tag is a claim, and carries who made it
--
-- Every row records the **model** that produced it and the confidence it claimed. That is
-- not bookkeeping: two models disagree, a model is upgraded, and a user who filtered on
-- "beach" deserves to know whether those tags came from a 35B vision model or from a
-- fallback that guessed. Without provenance, a re-tag silently mixes two definitions of
-- every word.
--
-- ## Confidence is stored, not thresholded away
--
-- The model's number is not a calibrated probability and is not treated as one. Storing it
-- means the UI can show it and a later filter can change its mind about where the line is —
-- which a boolean `is_beach` column could never do.
--
-- ## Keyed by photograph, not by file
--
-- Tags describe what is *in the photograph*, which is a fact about the picture rather than
-- about the raw or the JPEG. A re-export of the same frame must not need re-tagging.

CREATE TABLE tag (
    id          INTEGER PRIMARY KEY,
    photo_id    INTEGER NOT NULL REFERENCES photo (id) ON DELETE CASCADE,
    -- Normalised to lower case on write. "Beach", "beach" and "BEACH" are one tag, and a
    -- vocabulary that treats them as three is one nobody can filter.
    name        TEXT NOT NULL,
    confidence  REAL NOT NULL,
    -- Which model said so. See above.
    model       TEXT NOT NULL,
    -- A one-line description, when the model gave one. Stored per photograph rather than
    -- per tag, but kept here so a tag row is self-contained for export.
    description TEXT,
    tagged_at   INTEGER NOT NULL
);

-- One row per photograph per tag per model: a re-tag by the same model replaces rather than
-- accumulates, and a tag from a different model is kept alongside.
CREATE UNIQUE INDEX tag_unique ON tag (photo_id, name, model);
CREATE INDEX tag_by_name ON tag (name);
CREATE INDEX tag_by_photo ON tag (photo_id);

-- The description is one per photograph per model, not one per tag. A separate table would
-- be tidier and would also make "what did the model say about this?" a join for no reason.
CREATE TABLE photo_caption (
    photo_id    INTEGER NOT NULL REFERENCES photo (id) ON DELETE CASCADE,
    model       TEXT NOT NULL,
    description TEXT NOT NULL,
    tagged_at   INTEGER NOT NULL,
    PRIMARY KEY (photo_id, model)
);
