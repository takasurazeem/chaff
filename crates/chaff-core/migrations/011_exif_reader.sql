-- Chaff catalog, schema version 11: which EXIF reader produced a row.
--
-- ## The bug this exists to fix
--
-- `files_needing_exif` re-read a file only when its **modification time** changed. That is
-- right for a file that changed and wrong for a *reader* that changed: a library indexed
-- before the Canon CR3 fix keeps its "this file has no EXIF" rows forever, and the fix cannot
-- reach the files it was written for.
--
-- That is what happened. Every CR3 in a 2,956-photograph library said "no camera information"
-- after the reader was fixed, because the catalog was still serving the old answer.
--
-- ## The same pattern as `scorer_version`
--
-- `measurement` and `score` are keyed by `scorer_version` for exactly this reason, and the
-- comment on `SCORER_VERSION` says so. This is that pattern applied to the other cache.
--
-- A row written before this migration has no version. It is treated as stale — the reader has
-- changed at least once, so an unversioned row cannot be trusted to be current — and every
-- file is examined once more.

ALTER TABLE exif ADD COLUMN reader_version INTEGER;

-- The lookup `files_needing_exif` does, which would otherwise scan every file.
CREATE INDEX exif_by_version ON exif (reader_version);
