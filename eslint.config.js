// Lint configuration.
//
// Deliberately small. A large ruleset on a young codebase produces hundreds of findings
// that get bulk-suppressed, which is worse than no lint at all. These are the rules that
// catch things the compiler and the tests cannot:
//
//  * `react-hooks/exhaustive-deps` — a missing dependency is a stale closure, which is a
//    bug that looks like flakiness and is nearly impossible to find by reading.
//  * `react-hooks/rules-of-hooks` — a conditional hook is a crash.
//  * unused variables and unreachable code, which `tsc` leaves to the linter.
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";

export default tseslint.config(
  { ignores: ["dist", "node_modules", "src-tauri/target", "target"] },
  {
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2022,
      globals: globals.browser,
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // **Off, deliberately.** The rule wants a component file to export only components,
      // because exporting a helper alongside one breaks fast refresh for that file. True,
      // and not worth the cost here: the formatters and `buildTree` are exported so they can
      // be unit-tested in isolation, which is worth more than a dev-server reload.
      //
      // Recorded rather than silenced: the first attempt moved them into their own modules
      // and damaged six files doing it, which is a worse trade than a warning nobody sees.
      "react-refresh/only-export-components": "off",
      // An unused argument is often deliberate (a signature being satisfied), and flagging
      // it produces noise that buries the unused *variables*.
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
  {
    // Tests may reach for `any` and for helpers that exist only to build fixtures.
    files: ["**/*.test.{ts,tsx}"],
    rules: {
      "@typescript-eslint/no-explicit-any": "off",
    },
  },
);
