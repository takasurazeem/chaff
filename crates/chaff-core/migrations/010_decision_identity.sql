-- Chaff catalog, schema version 10: what a decision was made about.
--
-- ## The problem
--
-- A decision is keyed by `photo_id`, and a photograph is `(library_id, dir, stem)`. Rename
-- `IMG_0001.CR3` to `IMG_0001-edit.CR3` and the stem changes, so the next index creates a
-- **new** photograph — and the rating, the reject flag and the shoot grouping stay attached
-- to a row nothing points at any more. The user renamed a file and lost their work on it.
--
-- ## The identity recorded here
--
-- The primary file's **size and modification time**, taken when the decision was made. Both
-- survive a rename — `mv` does not touch either — and neither costs anything to record.
--
-- A content hash would be a stronger identity and is deliberately not used: hashing a 45 MB
-- raw on every rating is 90 ms per keystroke, and a culling session is thousands of
-- keystrokes. `content_hash` exists in `file` for the pre-move check, where it is computed
-- once per delete and the cost is worth paying.
--
-- ## Why adoption is restricted to orphans
--
-- A decision may be adopted by a new photograph only when the photograph it was made about
-- **no longer exists**. Without that rule two photographs of the same size and time — a
-- burst frame, a re-export — would both claim one rating, and the second would silently
-- overwrite the first.

ALTER TABLE decision ADD COLUMN source_size  INTEGER;
ALTER TABLE decision ADD COLUMN source_mtime INTEGER;

-- The lookup an orphan adoption does, which would otherwise scan every decision.
CREATE INDEX decision_by_identity ON decision (source_size, source_mtime);
