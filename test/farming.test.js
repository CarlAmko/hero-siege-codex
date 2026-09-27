import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DIFFICULTY_XP_BONUS,
  rewardRatio,
  breakEvenTimeRatio,
  shouldFarmHarder,
  summarizeFarmingSamples,
} from '../src/farming.js';

test('difficulty experience bonuses match Season 10 patch 7.0.3', () => {
  assert.equal(DIFFICULTY_XP_BONUS.hell, 0.15);
  assert.equal(DIFFICULTY_XP_BONUS.inferno, 0.2);
  assert.equal(DIFFICULTY_XP_BONUS.nightmare, 0.1);
});

test('Hell -> Inferno rewards +4.3% with no additive bonuses', () => {
  const ratio = rewardRatio({ from: 'hell', to: 'inferno' });
  assert.ok(Math.abs(ratio - 1.2 / 1.15) < 1e-12);
  assert.ok(Math.abs(ratio - 1.0435) < 0.0001);
});

test('a maxed additive pact slider dilutes the Inferno edge to +1.4%', () => {
  const ratio = rewardRatio({ from: 'hell', to: 'inferno', additivePool: 2.5 });
  assert.ok(Math.abs(ratio - 3.7 / 3.65) < 1e-12);
  assert.ok(Math.abs(ratio - 1.0137) < 0.0001);
});

test('additive bonuses dilute monotonically toward parity', () => {
  const none = rewardRatio({ from: 'hell', to: 'inferno' });
  const pact = rewardRatio({ from: 'hell', to: 'inferno', additivePool: 2.5 });
  const huge = rewardRatio({ from: 'hell', to: 'inferno', additivePool: 1000 });
  assert.ok(none > pact);
  assert.ok(pact > huge);
  assert.ok(huge > 1, 'a common additive bonus can never invert the ratio');
});

test('base reward ratio (monster tier) scales the difficulty delta', () => {
  const ratio = rewardRatio({ from: 'hell', to: 'inferno', baseRewardRatio: 1.1 });
  assert.ok(Math.abs(ratio - 1.1 * (1.2 / 1.15)) < 1e-12);
});

test('unknown difficulty ids contribute no bonus', () => {
  assert.ok(Math.abs(rewardRatio({ from: 'hard', to: 'inferno' }) - 1.2) < 1e-12);
});

test('break-even time ratio is the reward ratio', () => {
  const ratio = rewardRatio({ from: 'hell', to: 'inferno', additivePool: 2.5 });
  assert.equal(breakEvenTimeRatio(ratio), ratio);
});

test('maxed BP: 10% slower Inferno loses, 1% slower wins', () => {
  const ratio = rewardRatio({ from: 'hell', to: 'inferno', additivePool: 2.5 });
  const slower = shouldFarmHarder({ rewardRatio: ratio, clearTimeRatio: 1.1 });
  assert.equal(slower.tier, 'easier');
  assert.equal(slower.swap, false);
  assert.ok(slower.headroom < 0);

  const juiced = shouldFarmHarder({ rewardRatio: ratio, clearTimeRatio: 1.01 });
  assert.equal(juiced.tier, 'harder');
  assert.equal(juiced.swap, true);
});

test('exact parity favours the easier tier (strict comparison)', () => {
  const decision = shouldFarmHarder({ rewardRatio: 1.0137, clearTimeRatio: 1.0137 });
  assert.equal(decision.swap, false);
  assert.equal(decision.headroom, 0);
});

test('Inferno-gated loot beats any finite clear-time penalty', () => {
  const decision = shouldFarmHarder({ rewardRatio: Infinity, clearTimeRatio: 6 });
  assert.equal(decision.swap, true);
  assert.equal(decision.headroom, Infinity);
});

test('summarizeFarmingSamples buckets by difficulty and pact flag', () => {
  const samples = [
    { difficulty: 3, blood_pact: 0, kills: 100, experience: 1_000_000 },
    { difficulty: 3, blood_pact: 0, kills: 120, experience: 1_100_000 },
    { difficulty: 3, blood_pact: 0, kills: 160, experience: 1_250_000 },
    { difficulty: 2, blood_pact: 0, kills: 0, experience: 0 },
    { difficulty: 2, blood_pact: 0, kills: 30, experience: 60_000 },
    { difficulty: 2, blood_pact: 0, kills: 40, experience: 70_000 },
    { difficulty: 3, blood_pact: 1, kills: 0, experience: 0 },
    { difficulty: 3, blood_pact: 1, kills: 10, experience: 50_000 },
  ];

  const summary = summarizeFarmingSamples(samples, { durationMs: 3_600_000 });

  assert.deepEqual(Object.keys(summary).sort(), ['2', '3', '3+bp']);

  assert.equal(summary['3'].kills, 60);
  assert.equal(summary['3'].xp, 250_000);
  assert.ok(Math.abs(summary['3'].xpPerKill - 250_000 / 60) < 1e-9);
  assert.equal(summary['3'].killsPerHour, 60);
  assert.equal(summary['3'].xpPerHour, 250_000);

  assert.equal(summary['2'].kills, 40);
  assert.equal(summary['2'].xpPerKill, 1750);

  assert.equal(summary['3+bp'].xpPerKill, 5000);
});

test('summarizeFarmingSamples omits rates without a duration', () => {
  const summary = summarizeFarmingSamples(
    [
      { difficulty: 3, kills: 0, experience: 0 },
      { difficulty: 3, kills: 10, experience: 50_000 },
    ],
  );
  assert.equal(summary['3'].killsPerHour, null);
  assert.equal(summary['3'].xpPerHour, null);
  assert.equal(summary['3'].xpPerKill, 5000);
});

test('summarizeFarmingSamples ignores counter resets', () => {
  const summary = summarizeFarmingSamples(
    [
      { difficulty: 3, kills: 10, experience: 1000 },
      { difficulty: 3, kills: 9, experience: 900 },
    ],
  );
  assert.equal(summary['3'].kills, 0);
  assert.equal(summary['3'].xp, 0);
  assert.equal(summary['3'].xpPerKill, 0);
});
