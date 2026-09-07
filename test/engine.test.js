import { test } from 'node:test';
import assert from 'node:assert/strict';
import { findItem, searchItems } from '../src/database.js';
import { evaluateItem, VERDICTS } from '../src/engine.js';

test('Angelic SS items always trigger INSTANT_PRICE_CHECK', () => {
  const match = findItem("Lucifer's Crown");
  assert.ok(match, 'Found Lucifer\'s Crown');
  assert.equal(match.data.rarity, 'angelic');
  assert.equal(match.data.grade, 'SS');

  const evalResult = evaluateItem(match.data);
  assert.equal(evalResult.verdict.code, VERDICTS.INSTANT_PRICE_CHECK.code);
  assert.ok(evalResult.score >= 95, `Score should be >= 95, got ${evalResult.score}`);
  assert.equal(evalResult.estimatedBracket, 'Ultra High (Ruby/Gold Cap)');
});

test('Satanic SS items trigger INSTANT_PRICE_CHECK', () => {
  const match = findItem('The Bucket');
  assert.ok(match, 'Found The Bucket');
  assert.equal(match.data.rarity, 'satanic');
  assert.equal(match.data.grade, 'SS');

  const evalResult = evaluateItem(match.data);
  assert.equal(evalResult.verdict.code, VERDICTS.INSTANT_PRICE_CHECK.code);
  assert.ok(evalResult.score >= 80, `Score should be >= 80, got ${evalResult.score}`);
});

test('Satanic S items trigger CONDITIONAL_CHECK, upgraded to INSTANT on 115% quality', () => {
  const match = findItem("Harlequinn's Crest");
  assert.ok(match, 'Found Harlequinn\'s Crest');
  assert.equal(match.data.grade, 'S');

  // Standard 100%
  const normalEval = evaluateItem(match.data, { quality: 100 });
  assert.equal(normalEval.verdict.code, VERDICTS.CONDITIONAL_CHECK.code);

  // God Roll 115%
  const godEval = evaluateItem(match.data, { quality: 115 });
  assert.equal(godEval.verdict.code, VERDICTS.INSTANT_PRICE_CHECK.code);
  assert.equal(godEval.qualityMultiplier, 6.0);
});

test('Satanic A items default to RARE_EXCEPTION_DUST', () => {
  // Find a Grade A satanic item
  const results = searchItems('', { rarity: 'satanic', grade: 'A' });
  assert.ok(results.length > 0, 'Found Grade A items');
  const aItem = results[0];

  const normalEval = evaluateItem(aItem, { quality: 100 });
  assert.equal(normalEval.verdict.code, VERDICTS.RARE_EXCEPTION_DUST.code);

  // 115% quality upgrades it to CONDITIONAL_CHECK or INSTANT_PRICE_CHECK
  const godEval = evaluateItem(aItem, { quality: 115 });
  assert.ok(
    godEval.verdict.code === VERDICTS.CONDITIONAL_CHECK.code ||
    godEval.verdict.code === VERDICTS.INSTANT_PRICE_CHECK.code
  );
});

test('Satanic B, C, D items default to DUST_PROSPECT', () => {
  const dItems = searchItems('', { rarity: 'satanic', grade: 'D' });
  assert.ok(dItems.length > 0, 'Found Grade D items');
  const dItem = dItems[0];

  const evalResult = evaluateItem(dItem);
  assert.equal(evalResult.verdict.code, VERDICTS.DUST_PROSPECT.code);
  assert.ok(evalResult.reasons.some((r) => r.includes('crafting/crystal dust fodder')));
});

test('High Runes (Tier 3 & 4) trigger INSTANT_PRICE_CHECK', () => {
  const jah = findItem('Jah');
  assert.ok(jah, 'Found Jah rune');
  const jahEval = evaluateItem(jah.data);
  assert.equal(jahEval.verdict.code, VERDICTS.INSTANT_PRICE_CHECK.code);

  const fawn = findItem('Fawn');
  assert.ok(fawn, 'Found Fawn rune');
  const fawnEval = evaluateItem(fawn.data);
  assert.equal(fawnEval.verdict.code, VERDICTS.INSTANT_PRICE_CHECK.code);
});

test('Mid Runes (Tier 2) trigger CONDITIONAL_CHECK, Low Runes trigger DUST', () => {
  const ist = findItem('Ist');
  assert.ok(ist, 'Found Ist rune');
  const istEval = evaluateItem(ist.data);
  assert.equal(istEval.verdict.code, VERDICTS.CONDITIONAL_CHECK.code);

  const old = findItem('Old');
  assert.ok(old, 'Found Old rune');
  const oldEval = evaluateItem(old.data);
  assert.equal(oldEval.verdict.code, VERDICTS.DUST_PROSPECT.code);
});

test('Fuzzy name search finds items with partial queries', () => {
  const match1 = findItem('lucifer');
  assert.ok(match1, 'Matched "lucifer"');
  assert.equal(match1.data.name, "Lucifer's Crown");

  const match2 = findItem('bucket');
  assert.ok(match2, 'Matched "bucket"');
  assert.equal(match2.data.name, 'The Bucket');

  const match3 = findItem('hfs');
  assert.ok(match3, 'Matched "hfs"');
  assert.equal(match3.data.name, "Gut's HFS");
});
