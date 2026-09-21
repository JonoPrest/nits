// Boundary test (ARCHITECTURE §6.3): every protocol fixture parses with the
// hand-written Sury schema and re-serialises to the same JSON, and every
// fixture directory has a schema. Drift in either direction fails here.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
// Compiled by `pnpm rescript` before the tests run (see CI).
import * as Registry from "../src/protocol/Registry.res.mjs";
import * as ClientRegistry from "../src/view/ClientRegistry.res.mjs";

const fixturesRoot = join(__dirname, "..", "..", "fixtures");

/** Sort object keys recursively so JSON compares byte-for-byte. */
const canonical = (v: unknown): unknown => {
  if (Array.isArray(v)) return v.map(canonical);
  if (v && typeof v === "object") {
    return Object.fromEntries(
      Object.keys(v as object)
        .sort()
        .map((k) => [k, canonical((v as Record<string, unknown>)[k])]),
    );
  }
  return v;
};

type Reg = {
  names: string[];
  roundtrip: (type: string, json: unknown) => { TAG: string; _0: unknown };
};

const suites: Array<[string, Reg]> = [
  ["protocol", Registry as unknown as Reg],
  ["client", ClientRegistry as unknown as Reg],
];

for (const [set, registry] of suites) {
  const fixtures = join(fixturesRoot, set);
  const types = readdirSync(fixtures).filter((d) => !d.startsWith("."));

  describe(`${set} fixtures round-trip through the Sury schemas`, () => {
    it("has a schema for every fixture type", () => {
      const missing = types.filter((t) => !registry.names.includes(t));
      expect(missing).toEqual([]);
    });

    it("has a fixture directory for every schema", () => {
      const extra = registry.names.filter((n) => !types.includes(n));
      expect(extra).toEqual([]);
    });

    for (const type of types) {
      const files = readdirSync(join(fixtures, type)).filter((f) => f.endsWith(".json"));
      for (const file of files) {
        it(`${type}/${file}`, () => {
          const json = JSON.parse(readFileSync(join(fixtures, type, file), "utf8"));
          const result = registry.roundtrip(type, json);
          // ReScript `result`: {TAG: "Ok", _0} | {TAG: "Error", _0}
          if (result.TAG !== "Ok") throw new Error(String(result._0));
          expect(JSON.stringify(canonical(result._0))).toBe(JSON.stringify(canonical(json)));
        });
      }
    }
  });
}

it("directory bootstrap permits WorkingTree only on the head side", () => {
  const options = JSON.parse(
    readFileSync(join(fixturesRoot, "protocol", "EnsureDirectoryReview", "default.json"), "utf8"),
  );
  options.head = { type: "WorkingTree" };
  expect(Registry.roundtrip("EnsureDirectoryReview", options).TAG).toBe("Ok");
  options.base = { type: "WorkingTree" };
  expect(Registry.roundtrip("EnsureDirectoryReview", options).TAG).toBe("Error");
  const request = { type: "EnsureDirectoryReview", client_seq: 1, options };
  expect(Registry.roundtrip("Request", request).TAG).toBe("Error");
});

it("suggestion preview is a single response and never a streamed item", () => {
  const preview = JSON.parse(
    readFileSync(join(fixturesRoot, "protocol", "Response", "SuggestionPreview.json"), "utf8"),
  );
  expect(Registry.roundtrip("Response", preview).TAG).toBe("Ok");
  expect(Registry.roundtrip("StreamItem", preview).TAG).toBe("Error");
  expect(Registry.roundtrip("ServerMsg", { type: "Response", id: 1, response: preview }).TAG).toBe("Ok");
  expect(Registry.roundtrip("ServerMsg", { type: "StreamItem", id: 1, item: preview }).TAG).toBe("Error");
});


it("review discovery is a single metadata response, never a streamed snapshot", () => {
  const response = JSON.parse(
    readFileSync(join(fixturesRoot, "protocol", "Response", "ReviewDiscovery.json"), "utf8"),
  );
  expect(Registry.roundtrip("Response", response).TAG).toBe("Ok");
  expect(Registry.roundtrip("StreamItem", response).TAG).toBe("Error");
  expect(Registry.roundtrip("ServerMsg", { type: "Response", id: 7, response }).TAG).toBe("Ok");
  expect(Registry.roundtrip("ServerMsg", { type: "StreamItem", id: 7, item: response }).TAG).toBe("Error");
  response.discovery.reviews[0].pending_requests[0].recipient = 7;
  expect(Registry.roundtrip("Response", response).TAG).toBe("Error");
});
