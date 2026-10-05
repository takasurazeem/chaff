/**
 * The library as folders.
 *
 * A photographer's library is organised by folder — a shoot, a trip, a date — and that
 * organisation is the fastest filter there is. Without this the grid is one flat list of
 * three thousand photographs with no way in except scrolling.
 *
 * ## Built here, not in the engine
 *
 * `list_directories` returns a flat list of paths with counts. Turning that into a tree is
 * a display concern: the engine has no opinion about indentation, expansion state or
 * whether a folder is shown at all, and a tree assembled there would have to be rebuilt
 * every time any of that changed.
 *
 * ## Both counts, because they answer different questions
 *
 * `direct` is "how many are in this shoot?" — the number that matters when you are looking
 * at a folder of files. `recursive` is "how many are under 2024?" — the number that matters
 * when you are navigating. A tree showing only direct counts makes every parent look empty.
 */
import { useMemo, useState } from "react";
import type { DirectoryView } from "../api";

interface Node {
  name: string;
  path: string;
  direct: number;
  recursive: number;
  children: Node[];
  depth: number;
}

/**
 * Assemble the flat list into a tree.
 *
 * The query returns only folders that **hold** photographs, so a parent with nothing
 * directly in it — `2024/` above `2024/Iceland/` — is absent. It still has to appear, or
 * its children float at the root and the hierarchy is lost.
 *
 * `ensureChain` builds the whole ancestor path, not just the immediate parent. The first
 * version created one missing level and attached it to the root, so `/lib/a/b/c` came out
 * two levels deep instead of four — a tree that looked plausible and was wrong.
 */
export function buildTree(rows: DirectoryView[]): Node[] {
  const byPath = new Map<string, Node>();
  const roots: Node[] = [];

  const nameOf = (path: string) => path.split("/").filter(Boolean).pop() ?? path;
  const depthOf = (path: string) => path.split("/").filter(Boolean).length;

  function ensureChain(path: string): Node {
    const existing = byPath.get(path);
    if (existing) return existing;

    const node: Node = {
      name: nameOf(path),
      path,
      direct: 0,
      recursive: 0,
      children: [],
      depth: depthOf(path),
    };
    byPath.set(path, node);

    const parentPath = path.replace(/\/[^/]+$/, "");
    if (parentPath && parentPath !== path) {
      ensureChain(parentPath).children.push(node);
    } else {
      roots.push(node);
    }
    return node;
  }

  for (const row of rows) {
    const node = ensureChain(row.path);
    node.direct = row.direct;
    node.recursive = row.recursive;
  }

  const sort = (nodes: Node[]) => {
    nodes.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
    nodes.forEach((n) => sort(n.children));
  };
  sort(roots);
  return roots;
}

interface Props {
  rows: DirectoryView[];
  root: string;
  /** The selected folder path, or null for the whole library. */
  selected: string | null;
  total: number;
  onSelect: (path: string | null) => void;
}

function Row({
  node,
  selected,
  onSelect,
  expanded,
  toggle,
}: {
  node: Node;
  selected: string | null;
  onSelect: (p: string | null) => void;
  expanded: Set<string>;
  toggle: (p: string) => void;
}) {
  const open = expanded.has(node.path);
  const hasChildren = node.children.length > 0;
  const active = selected === node.path;

  return (
    <>
      <div
        className={[
          "flex items-center gap-1 rounded pr-1 text-xs",
          active ? "bg-sky-500/20 text-sky-100" : "text-zinc-300 hover:bg-zinc-800",
        ].join(" ")}
        style={{ paddingLeft: `${node.depth * 10}px` }}
      >
        <button
          type="button"
          onClick={() => hasChildren && toggle(node.path)}
          aria-label={hasChildren ? (open ? `Collapse ${node.name}` : `Expand ${node.name}`) : undefined}
          aria-expanded={hasChildren ? open : undefined}
          className={`min-h-6 w-4 shrink-0 rounded text-[10px] ${
            hasChildren ? "text-zinc-400 hover:text-zinc-100" : "invisible"
          } focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400`}
          tabIndex={hasChildren ? 0 : -1}
        >
          {open ? "▾" : "▸"}
        </button>

        <button
          type="button"
          onClick={() => onSelect(active ? null : node.path)}
          aria-pressed={active}
          title={`${node.path}\n${node.direct} directly, ${node.recursive} including subfolders`}
          className="flex min-h-6 min-w-0 flex-1 items-center gap-2 rounded text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400"
        >
          <span className="truncate">{node.name}</span>
          <span className="ml-auto shrink-0 tabular-nums text-zinc-500">
            {node.recursive.toLocaleString()}
          </span>
        </button>
      </div>

      {open &&
        node.children.map((c) => (
          <Row
            key={c.path}
            node={c}
            selected={selected}
            onSelect={onSelect}
            expanded={expanded}
            toggle={toggle}
          />
        ))}
    </>
  );
}

export function FolderTree({ rows, root, selected, total, onSelect }: Props) {
  const tree = useMemo(() => buildTree(rows), [rows]);
  // Top-level folders start open: a library whose tree is entirely collapsed on open looks
  // empty, and the first thing anyone does is expand all of it.
  const [expanded, setExpanded] = useState<Set<string>>(
    () => new Set(tree.map((n) => n.path)),
  );

  const toggle = (path: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  const rootName = root.split("/").filter(Boolean).pop() ?? root;

  return (
    <nav
      aria-label="Folders"
      className="flex w-56 shrink-0 flex-col overflow-y-auto border-r border-zinc-800 bg-zinc-950 py-1"
    >
      <button
        type="button"
        onClick={() => onSelect(null)}
        aria-pressed={selected === null}
        className={[
          "mx-1 flex min-h-6 items-center gap-2 rounded px-2 text-left text-xs",
          selected === null ? "bg-sky-500/20 text-sky-100" : "text-zinc-300 hover:bg-zinc-800",
          "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400",
        ].join(" ")}
      >
        <span className="truncate font-medium">{rootName}</span>
        <span className="ml-auto shrink-0 tabular-nums text-zinc-500">
          {total.toLocaleString()}
        </span>
      </button>

      <div className="mt-0.5 px-1">
        {tree.map((n) => (
          <Row
            key={n.path}
            node={n}
            selected={selected}
            onSelect={onSelect}
            expanded={expanded}
            toggle={toggle}
          />
        ))}
      </div>

      {tree.length === 0 && (
        <p className="px-3 py-2 text-[11px] text-zinc-500">No folders.</p>
      )}
    </nav>
  );
}
