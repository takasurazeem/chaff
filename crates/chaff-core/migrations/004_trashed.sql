-- Chaff catalog, schema version 4: a photograph that is in the trash.
--
-- ## Why the row survives
--
-- When a photograph is trashed its files leave the library. The obvious next step is to
-- remove the row, and the indexer's sweep would do exactly that — but `decision` cascades
-- with `photo`, so **removing the row would take the user's rating with it**, and a
-- restore would bring the file back unrated.
--
-- So a trashed photograph keeps its row and gains a timestamp. Three consequences:
--
-- 1. The rating, the reject flag and the score all survive a trash/restore round trip.
-- 2. `photos()` hides it, so the grid behaves as if it were gone.
-- 3. The indexer's sweep skips it, so an index pass over a library with a full trash does
--    not quietly destroy every decision in it.
--
-- ## It is not a second trash
--
-- The files live in `.cull-trash`; this column only records that they do. If the trash
-- folder is emptied by hand, the row remains and the next index pass sweeps it — which is
-- the correct outcome, because the photograph really is gone.

ALTER TABLE photo ADD COLUMN trashed_at INTEGER;

-- The grid asks for the untrashed photographs in a library. Without this the query scans.
CREATE INDEX photo_untrashed ON photo (library_id, trashed_at);
