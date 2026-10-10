#!/usr/bin/env node
// Generate the market catalog hs-sniff embeds for name -> market-target
// resolution. The market API only takes numeric identities (an item mask or a
// runeword repository id), so a plain-text query needs a name table.
//
// Source: the vendored Hero Siege Companion identity extract
// (refs/Hero-Siege-Companion/src/shared/data/items/build-24868792). That is the
// only retained source that carries the `repository` bit the mask needs —
// hs-tracker's packed (type,id,weaponType) drops it and collides unique with
// normal items. Re-run this after bumping the Companion submodule:
//
//   node hs-sniff/tools/gen-market-catalog.mjs
//
// Output (committed, embedded via include_str!):
//   hs-sniff/data/market-items.tsv      repository  type  gameId  weaponType  name
//   hs-sniff/data/market-runewords.tsv  id          name
//
// Not part of any runtime path; the TSVs are the shipped artifact.

import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "..", "..");
const dataDir = path.join(repo, "refs", "Hero-Siege-Companion", "src", "shared", "data", "items", "build-24868792");
const outDir = path.join(repo, "hs-sniff", "data");

const identities = JSON.parse(readFileSync(path.join(dataDir, "identities.json"), "utf8"));
const runewords = JSON.parse(readFileSync(path.join(dataDir, "runewords.json"), "utf8"));
const itemStats = JSON.parse(readFileSync(path.join(dataDir, "item-stats.json"), "utf8"));

const nameOf = (definition) => definition.name ?? definition.baseName ?? null;

const items = identities.definitions
  .filter((definition) => definition.repository === "normal" || definition.repository === "unique")
  .filter((definition) => nameOf(definition))
  .map((definition) => [
    definition.repository,
    definition.type,
    definition.gameId,
    definition.weaponType,
    nameOf(definition),
  ])
  .sort((left, right) => left[4].localeCompare(right[4]) || left[0].localeCompare(right[0]));

// The binding key is `<repository>:<type>:<weaponType>:<gameId>`, matching the
// identity tuple exactly; join through it to keep the permutation honest.
const runewordNames = new Map(
  identities.definitions
    .filter((definition) => definition.repository === "runeword")
    .map((definition) => [
      `${definition.repository}:${definition.type}:${definition.weaponType}:${definition.gameId}`,
      nameOf(definition),
    ]),
);

const runes = runewords.bindings
  .map(([id, key]) => [id, runewordNames.get(key)])
  .filter(([, name]) => name)
  .sort((left, right) => left[0] - right[0]);

// Only stats with a real display name; "Stat 482" is a placeholder, not a
// phrase a player types. Native Market-menu names win over experimental ones
// that happen to share an id.
const stats = itemStats.stats
  .filter((stat) => stat.name && !/^Stat \d+$/.test(stat.name))
  .map((stat) => [stat.statId, stat.name, stat.nativeMarketMenu === true ? 1 : 0])
  .sort((left, right) => left[1].localeCompare(right[1]) || right[2] - left[2]);

writeFileSync(
  path.join(outDir, "market-items.tsv"),
  "repository\ttype\tgameId\tweaponType\tname\n" + items.map((row) => row.join("\t")).join("\n") + "\n",
);
writeFileSync(
  path.join(outDir, "market-runewords.tsv"),
  "id\tname\n" + runes.map((row) => row.join("\t")).join("\n") + "\n",
);
writeFileSync(
  path.join(outDir, "market-stats.tsv"),
  "id\tname\n" + stats.map((row) => `${row[0]}\t${row[1]}`).join("\n") + "\n",
);

console.log(`market-items.tsv: ${items.length} identities`);
console.log(`market-runewords.tsv: ${runes.length} runewords`);
console.log(`market-stats.tsv: ${stats.length} named stats`);
