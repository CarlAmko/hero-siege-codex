import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const dataDir = join(__dirname, '..', 'data');

const loadJson = (filename) => {
  try {
    const raw = readFileSync(join(dataDir, filename), 'utf-8');
    return JSON.parse(raw);
  } catch (err) {
    console.error(`Failed to load ${filename}:`, err.message);
    return [];
  }
};

export const items = loadJson('unified-items.json');
export const runes = loadJson('runes.json');
export const runewords = loadJson('runewords.json');
export const sets = loadJson('sets.json');

// Normalizer for fast fuzzy & token lookups
export function normalizeKey(str) {
  if (!str) return '';
  return str
    .toLowerCase()
    .replace(/['’"]/g, '')
    .replace(/[^a-z0-9]+/g, ' ')
    .trim();
}

// Maps
const itemsById = new Map();
const itemsByName = new Map();

for (const item of items) {
  if (item.id) itemsById.set(item.id, item);
  if (item.name) {
    itemsByName.set(normalizeKey(item.name), item);
  }
}

const runesByName = new Map();
for (const rune of runes) {
  if (rune.name) {
    runesByName.set(normalizeKey(rune.name), rune);
    runesByName.set(normalizeKey(`rune ${rune.name}`), rune);
    runesByName.set(normalizeKey(`${rune.name} rune`), rune);
  }
}

const runewordsByName = new Map();
for (const rw of runewords) {
  if (rw.name) {
    runewordsByName.set(normalizeKey(rw.name), rw);
  }
}

/**
 * Find exact or best matching item/rune/runeword
 */
export function findItem(query) {
  if (!query) return null;
  const key = normalizeKey(query);

  // Exact lookups
  if (itemsByName.has(key)) return { type: 'item', data: itemsByName.get(key) };
  if (itemsById.has(query)) return { type: 'item', data: itemsById.get(query) };
  if (runesByName.has(key)) return { type: 'rune', data: runesByName.get(key) };
  if (runewordsByName.has(key)) return { type: 'runeword', data: runewordsByName.get(key) };

  // Prefix / substring match
  for (const [nameKey, item] of itemsByName.entries()) {
    if (nameKey.startsWith(key) || nameKey.includes(key)) {
      return { type: 'item', data: item };
    }
  }

  for (const [nameKey, rune] of runesByName.entries()) {
    if (nameKey.includes(key)) {
      return { type: 'rune', data: rune };
    }
  }

  for (const [nameKey, rw] of runewordsByName.entries()) {
    if (nameKey.includes(key)) {
      return { type: 'runeword', data: rw };
    }
  }

  return null;
}

/**
 * Search items by query with filters
 */
export function searchItems(query, filters = {}) {
  const key = normalizeKey(query);
  const results = [];

  for (const item of items) {
    if (filters.rarity && item.rarity !== filters.rarity) continue;
    if (filters.grade && item.grade !== filters.grade) continue;
    if (filters.slot && item.slot !== filters.slot) continue;

    if (!key) {
      results.push(item);
      continue;
    }

    const nameKey = normalizeKey(item.name);
    const baseKey = normalizeKey(item.baseType);

    if (nameKey.includes(key) || baseKey.includes(key)) {
      // Score relevance
      let score = 0;
      if (nameKey === key) score = 100;
      else if (nameKey.startsWith(key)) score = 80;
      else if (nameKey.includes(key)) score = 50;
      else score = 20;

      results.push({ item, score });
    }
  }

  if (key) {
    results.sort((a, b) => b.score - a.score);
    return results.map((r) => r.item);
  }

  return results;
}
