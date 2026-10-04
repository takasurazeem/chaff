-- Chaff catalog, schema version 3: the user's own decisions.
--
-- Everything before this table is derived. `score` is computed from pixels and can be
-- recomputed; `file` describes what is on disk and can be re-read. This table is the
-- first thing in the catalog that exists **nowhere else**. Lose it and the user has lost
-- hours of judgement that cannot be recovered by re-indexing.
--
-- That asymmetry drives three choices:
--
-- 1. **It is separate from `score`.** Re-scoring at a new `scorer_version` must not touch
--    a single row here. A metric change is not a reason for a rating to move.
-- 2. **It cascades only with the photograph.** Deleting a photograph removes its decision;
--    nothing else does.
-- 3. **There is no automatic writer.** Nothing in the engine sets a rating. A decision is
--    only ever the result of a person pressing a key.
--
-- ## A known limitation, recorded rather than hidden
--
-- Decisions are keyed by `photo_id`, and a photograph's identity is
-- `(library_id, dir, stem)`. **Renaming or moving a file therefore creates a new
-- photograph and orphans its decision.** Re-indexing sweeps the old row and the cascade
-- takes the rating with it.
--
-- The rename-proof alternative is to key on the content hash of the primary file, the
-- same trick the thumbnail cache uses. It is not done here because the grid needs the
-- decisions for fifty thousand photographs at once, and a content hash is filled lazily —
-- keying on it would mean hashing the whole library before the first frame could be
-- drawn. That is a solvable problem (hash in the background, migrate decisions as hashes
-- arrive) and it is issue #58 rather than something to rush into a schema.
--
-- Until then: rating a library and then reorganising it loses the ratings. Worth saying
-- out loud, because it is exactly the kind of thing a user discovers the hard way.

CREATE TABLE decision (
    photo_id   INTEGER PRIMARY KEY REFERENCES photo (id) ON DELETE CASCADE,
    -- 0 means unrated, which is distinct from a one-star rating. Every other value is the
    -- star count a photographer would recognise.
    rating     INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
    -- A flag beside the stars, not a rating value: a rejected photograph keeps whatever
    -- rating it had, which is what every photo tool does and what the user expects.
    rejected   INTEGER NOT NULL DEFAULT 0 CHECK (rejected IN (0, 1)),
    decided_at INTEGER NOT NULL
);

-- The grid asks for every decision in a library at once. Without this the query is a
-- scan per page of cells.
CREATE INDEX decision_decided ON decision (decided_at);
