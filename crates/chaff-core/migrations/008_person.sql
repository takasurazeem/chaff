-- Chaff catalog, schema version 8: embeddings, and the groups built from them.
--
-- ## An embedding is not an identity
--
-- `face_embedding` holds 128 floats per face. Two of them being close means the recogniser
-- thinks these are the same face — not that they are the same person. A `person` row is a
-- *suggestion*: a group of faces that might be one individual, awaiting confirmation.
--
-- Nothing writes a name here. #46 is the naming UI, and it is where a human turns a cluster
-- into a person. Keeping the two apart in the schema is what stops a name from being
-- attached to a face by a clustering run.
--
-- ## Why clusters are stored and not recomputed
--
-- Clustering is deterministic, so recomputing gives the same answer — for the same input.
-- But a user who has merged two groups and split a third has changed the answer, and
-- recomputing on every launch would discard exactly the corrections that make the feature
-- usable. A cluster carries `confirmed` for that reason: a confirmed group is not touched
-- by the next pass.

CREATE TABLE face_embedding (
    face_id     INTEGER PRIMARY KEY REFERENCES face (id) ON DELETE CASCADE,
    -- 128 f32, little-endian. A blob rather than 128 columns: nothing queries an individual
    -- component, and the only operation is "compare this whole vector to that one".
    vector      BLOB NOT NULL,
    model       TEXT NOT NULL,
    computed_at INTEGER NOT NULL
);

-- A suggested person: a group of faces that might be one individual.
CREATE TABLE person (
    id           INTEGER PRIMARY KEY,
    library_id   INTEGER NOT NULL REFERENCES library (id) ON DELETE CASCADE,
    -- Null until a human names it. The absence of a name is the normal state.
    name         TEXT,
    -- Set once the user has confirmed or corrected this group. A confirmed group is left
    -- alone by the next clustering pass, because the corrections are the point.
    confirmed    INTEGER NOT NULL DEFAULT 0 CHECK (confirmed IN (0, 1)),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE person_face (
    person_id INTEGER NOT NULL REFERENCES person (id) ON DELETE CASCADE,
    face_id   INTEGER NOT NULL REFERENCES face (id) ON DELETE CASCADE,
    PRIMARY KEY (person_id, face_id)
);

CREATE INDEX person_face_by_face ON person_face (face_id);
CREATE INDEX person_library ON person (library_id);
