import { describe, expect, it } from "vitest";
import { buildTree } from "./FolderTree";
import type { DirectoryView } from "../api";

const rows = (...r: Array<[string, number, number]>): DirectoryView[] =>
  r.map(([path, direct, recursive]) => ({ path, direct, recursive }));

describe("buildTree", () => {
  it("nests children under their parents", () => {
    const t = buildTree(
      rows(["/lib", 0, 5], ["/lib/2024", 0, 3], ["/lib/2024/Iceland", 3, 3], ["/lib/2023", 2, 2]),
    );
    expect(t).toHaveLength(1);
    expect(t[0].name).toBe("lib");
    expect(t[0].children.map((c) => c.name)).toEqual(["2023", "2024"]);
    expect(t[0].children[1].children[0].name).toBe("Iceland");
  });

  it("keeps both counts, because they answer different questions", () => {
    // `direct` is "how many in this shoot?"; `recursive` is "how many under 2024?". A tree
    // showing only direct counts makes every parent look empty.
    const t = buildTree(rows(["/lib", 0, 3], ["/lib/2024", 0, 3], ["/lib/2024/Iceland", 3, 3]));
    const y2024 = t[0].children[0];
    expect(y2024.direct).toBe(0);
    expect(y2024.recursive).toBe(3);
  });

  it("invents a parent the query did not return", () => {
    // The query only returns folders holding photographs, but a parent must still appear
    // or the child would float at the root. A tree with holes is not a tree.
    const t = buildTree(rows(["/lib/a/b/c", 4, 4]));
    expect(t).toHaveLength(1);
    expect(t[0].name).toBe("lib");
    expect(t[0].children[0].name).toBe("a");
    expect(t[0].children[0].children[0].name).toBe("b");
    expect(t[0].children[0].children[0].children[0].name).toBe("c");
  });

  it("sorts naturally, so 2 comes before 10", () => {
    const t = buildTree(rows(["/lib/x", 1, 1], ["/lib/10", 1, 1], ["/lib/2", 1, 1]));
    expect(t[0].children.map((c) => c.name)).toEqual(["2", "10", "x"]);
  });

  it("handles an empty library", () => {
    expect(buildTree([])).toEqual([]);
  });

  it("records depth, which the indentation depends on", () => {
    const t = buildTree(rows(["/lib", 0, 1], ["/lib/a", 0, 1], ["/lib/a/b", 1, 1]));
    expect(t[0].depth).toBe(1);
    expect(t[0].children[0].depth).toBe(2);
    expect(t[0].children[0].children[0].depth).toBe(3);
  });

  it("does not lose a folder whose parent is missing from the list", () => {
    // Every photograph must be reachable. A folder dropped because its parent was absent
    // is a folder the user cannot navigate to.
    const t = buildTree(rows(["/lib/keep/me", 2, 2], ["/lib/other/deep", 3, 3]));
    const flat: string[] = [];
    const walk = (n: any) => { flat.push(n.name); n.children.forEach(walk); };
    t.forEach(walk);
    expect(flat).toContain("me");
    expect(flat).toContain("deep");
  });
});
